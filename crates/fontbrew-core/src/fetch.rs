use std::{fmt, path::Path, sync::Arc, time::Duration};

use tokio::io::AsyncWriteExt;

use crate::{
    error::{FontbrewError, Result},
    model::{ensure_not_cancelled, CancellationToken},
};

const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const GITHUB_API_BASE_URL: &str = "https://api.github.com";
const FONTSOURCE_API_BASE_URL: &str = "https://api.fontsource.org/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequest {
    pub url: String,
    pub display_url: Option<String>,
    pub headers: Vec<HttpHeader>,
}

impl HttpRequest {
    pub fn display_url(&self) -> &str {
        self.display_url.as_deref().unwrap_or(&self.url)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub struct NetworkClient {
    client: reqwest::Client,
    endpoints: NetworkEndpoints,
}

#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct NetworkEndpoints {
    pub github_api_base_url: String,
    pub fontsource_api_base_url: String,
}

impl Default for NetworkEndpoints {
    fn default() -> Self {
        Self {
            github_api_base_url: GITHUB_API_BASE_URL.to_string(),
            fontsource_api_base_url: FONTSOURCE_API_BASE_URL.to_string(),
        }
    }
}

impl NetworkClient {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(DEFAULT_HTTP_TIMEOUT)
            .build()
            .map_err(|source| FontbrewError::Network {
                message: format!("could not build HTTP client: {source}"),
            })?;

        Ok(Self::with_client(client))
    }

    pub fn with_client(client: reqwest::Client) -> Self {
        Self::with_client_and_endpoints(client, NetworkEndpoints::default())
    }

    #[doc(hidden)]
    pub fn with_client_and_endpoints(client: reqwest::Client, endpoints: NetworkEndpoints) -> Self {
        Self {
            client,
            endpoints: NetworkEndpoints {
                github_api_base_url: endpoints
                    .github_api_base_url
                    .trim_end_matches('/')
                    .to_string(),
                fontsource_api_base_url: endpoints
                    .fontsource_api_base_url
                    .trim_end_matches('/')
                    .to_string(),
            },
        }
    }

    pub(crate) fn github_api_base_url(&self) -> &str {
        &self.endpoints.github_api_base_url
    }

    pub(crate) fn fontsource_api_base_url(&self) -> &str {
        &self.endpoints.fontsource_api_base_url
    }

    pub async fn get(&self, request: HttpRequest) -> Result<HttpResponse> {
        let mut builder = self.client.get(&request.url);
        for header in &request.headers {
            builder = builder.header(&header.name, &header.value);
        }

        let response = builder.send().await.map_err(|source| {
            let source = request_error_source(&request, source);
            FontbrewError::Network {
                message: format!("could not fetch {}: {source}", request.display_url()),
            }
        })?;
        let status = response.status().as_u16();
        let body = response.bytes().await.map_err(|source| {
            let source = request_error_source(&request, source);
            FontbrewError::Network {
                message: format!(
                    "could not read response body from {}: {source}",
                    request.display_url()
                ),
            }
        })?;

        Ok(HttpResponse {
            status,
            body: body.to_vec(),
        })
    }

    pub async fn download_to_file(
        &self,
        request: HttpRequest,
        destination: &Path,
        max_bytes: u64,
        cancellation: Arc<dyn CancellationToken>,
    ) -> Result<u64> {
        ensure_not_cancelled(cancellation.as_ref())?;

        let mut builder = self.client.get(&request.url);
        for header in &request.headers {
            builder = builder.header(&header.name, &header.value);
        }

        let mut response = builder.send().await.map_err(|source| {
            let source = request_error_source(&request, source);
            FontbrewError::Network {
                message: format!("could not fetch {}: {source}", request.display_url()),
            }
        })?;
        let status = response.status();
        if !status.is_success() {
            return Err(FontbrewError::Network {
                message: format!(
                    "HTTP request failed with status {status} for {}",
                    request.display_url()
                ),
            });
        }

        if let Some(content_length) = response.content_length() {
            reject_oversized_download(content_length, max_bytes, request.display_url())?;
        }

        if let Some(parent) = destination.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let mut destination_file = tokio::fs::File::create(destination).await?;
        let result = async {
            let mut downloaded = 0_u64;
            loop {
                ensure_not_cancelled(cancellation.as_ref())?;
                let chunk = response.chunk().await.map_err(|source| {
                    let source = request_error_source(&request, source);
                    FontbrewError::Network {
                        message: format!(
                            "could not read response body from {}: {source}",
                            request.display_url()
                        ),
                    }
                })?;
                let Some(chunk) = chunk else {
                    destination_file.flush().await?;
                    return Ok(downloaded);
                };

                ensure_not_cancelled(cancellation.as_ref())?;
                let next_downloaded =
                    downloaded.checked_add(chunk.len() as u64).ok_or_else(|| {
                        FontbrewError::ArchiveRejected {
                            reason: format!(
                                "download size overflowed for {}",
                                request.display_url()
                            ),
                        }
                    })?;
                reject_oversized_download(next_downloaded, max_bytes, request.display_url())?;
                destination_file.write_all(&chunk).await?;
                ensure_not_cancelled(cancellation.as_ref())?;
                downloaded = next_downloaded;
            }
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_file(destination).await;
        }

        result
    }
}

impl fmt::Debug for NetworkClient {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NetworkClient")
            .field("client", &"<reqwest-client>")
            .field("endpoints", &self.endpoints)
            .finish()
    }
}

fn request_error_source(request: &HttpRequest, source: impl fmt::Display) -> String {
    let message = source.to_string();
    let display_url = request.display_url();
    if display_url == request.url {
        return message;
    }

    message.replace(&request.url, display_url)
}

fn reject_oversized_download(downloaded: u64, max_bytes: u64, url: &str) -> Result<()> {
    if downloaded <= max_bytes {
        return Ok(());
    }

    Err(FontbrewError::ArchiveRejected {
        reason: format!("download exceeds maximum size of {max_bytes} bytes: {url}"),
    })
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Write},
        net::{Shutdown, TcpListener},
        sync::{mpsc, Arc},
        thread,
        time::Duration,
    };

    use crate::{model::NoCancellation, Result};

    use super::{request_error_source, HttpHeader, HttpRequest, NetworkClient};

    fn spawn_http_server(response: &'static [u8]) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test HTTP server");
        let address = listener.local_addr().expect("test HTTP server address");
        let (request_sender, request_receiver) = mpsc::channel();

        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept test request");
            let mut reader = BufReader::new(stream.try_clone().expect("clone test stream"));
            let mut request = String::new();
            loop {
                let mut line = String::new();
                let read = reader.read_line(&mut line).expect("read request line");
                if read == 0 || line == "\r\n" {
                    break;
                }
                request.push_str(&line);
            }
            request_sender.send(request).expect("send captured request");
            stream.write_all(response).expect("write test response");
            stream.flush().expect("flush test response");
            stream
                .shutdown(Shutdown::Write)
                .expect("shutdown test response");
        });

        (format!("http://{address}"), request_receiver)
    }

    #[test]
    fn request_error_source_uses_redacted_display_url() {
        let request = HttpRequest {
            url: "https://api.example.test/fonts?family=Inter&key=test-api-key".to_string(),
            display_url: Some(
                "https://api.example.test/fonts?family=Inter&key=<redacted>".to_string(),
            ),
            headers: Vec::new(),
        };

        let message = request_error_source(&request, format!("request failed for {}", request.url));

        assert!(!message.contains("test-api-key"));
        assert!(message.contains("key=<redacted>"));
    }

    #[tokio::test]
    async fn network_client_get_sends_headers_and_reads_response() -> Result<()> {
        let (url, requests) = spawn_http_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello",
        );
        let client = NetworkClient::with_client(reqwest::Client::new());

        let response = client
            .get(HttpRequest {
                url,
                display_url: None,
                headers: vec![HttpHeader {
                    name: "x-fontbrew-test".to_string(),
                    value: "yes".to_string(),
                }],
            })
            .await?;

        let request = requests
            .recv_timeout(Duration::from_secs(5))
            .expect("test server should capture request");
        assert!(request.contains("x-fontbrew-test: yes"));
        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hello");
        Ok(())
    }

    #[tokio::test]
    async fn network_client_download_to_file_writes_body_and_returns_byte_count() -> Result<()> {
        let (url, _requests) = spawn_http_server(
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world",
        );
        let temp = tempfile::tempdir()?;
        let destination = temp.path().join("download/font.ttf");
        let client = NetworkClient::with_client(reqwest::Client::new());

        let downloaded = client
            .download_to_file(
                HttpRequest {
                    url,
                    display_url: None,
                    headers: Vec::new(),
                },
                &destination,
                64,
                Arc::new(NoCancellation),
            )
            .await?;

        assert_eq!(downloaded, 11);
        assert_eq!(std::fs::read(destination)?, b"hello world");
        Ok(())
    }

    #[tokio::test]
    async fn network_client_download_to_file_rejects_http_error_without_destination() -> Result<()>
    {
        let (url, _requests) = spawn_http_server(
            b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 5\r\nConnection: close\r\n\r\nerror",
        );
        let temp = tempfile::tempdir()?;
        let destination = temp.path().join("download/font.ttf");
        let client = NetworkClient::with_client(reqwest::Client::new());

        let error = client
            .download_to_file(
                HttpRequest {
                    url,
                    display_url: None,
                    headers: Vec::new(),
                },
                &destination,
                64,
                Arc::new(NoCancellation),
            )
            .await
            .expect_err("HTTP error status should fail download");

        assert!(matches!(error, crate::FontbrewError::Network { .. }));
        assert!(!destination.exists());
        Ok(())
    }

    #[tokio::test]
    async fn network_client_download_to_file_removes_partial_file_when_chunks_exceed_limit(
    ) -> Result<()> {
        let (url, _requests) = spawn_http_server(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\nhell\r\n7\r\no world\r\n0\r\n\r\n",
        );
        let temp = tempfile::tempdir()?;
        let destination = temp.path().join("download/font.ttf");
        let client = NetworkClient::with_client(reqwest::Client::new());

        let error = client
            .download_to_file(
                HttpRequest {
                    url,
                    display_url: None,
                    headers: Vec::new(),
                },
                &destination,
                5,
                Arc::new(NoCancellation),
            )
            .await
            .expect_err("oversized chunked download should fail");

        assert!(matches!(
            error,
            crate::FontbrewError::ArchiveRejected { .. }
        ));
        assert!(!destination.exists());
        Ok(())
    }

    #[tokio::test]
    async fn network_client_download_to_file_writes_chunked_response_and_returns_byte_count(
    ) -> Result<()> {
        let (url, _requests) = spawn_http_server(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\nhello \r\n5\r\nworld\r\n0\r\n\r\n",
        );
        let temp = tempfile::tempdir()?;
        let destination = temp.path().join("download/font.ttf");
        let client = NetworkClient::with_client(reqwest::Client::new());

        let downloaded = client
            .download_to_file(
                HttpRequest {
                    url,
                    display_url: None,
                    headers: Vec::new(),
                },
                &destination,
                64,
                Arc::new(NoCancellation),
            )
            .await?;

        assert_eq!(downloaded, 11);
        assert_eq!(std::fs::read(destination)?, b"hello world");
        Ok(())
    }
}
