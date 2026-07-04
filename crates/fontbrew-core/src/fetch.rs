use std::{
    fmt,
    fs::{self, File},
    io::{Read, Write},
    path::Path,
    time::Duration,
};

use crate::{
    error::{FontbrewError, Result},
    model::{ensure_not_cancelled, CancellationToken},
};

const DEFAULT_HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_BUFFER_SIZE: usize = 64 * 1024;

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

pub trait HttpClient: Send + Sync {
    fn get(&self, request: HttpRequest) -> Result<HttpResponse>;

    fn download_to_file(
        &self,
        request: HttpRequest,
        destination: &Path,
        max_bytes: u64,
        cancellation: &dyn CancellationToken,
    ) -> Result<u64>;
}

#[derive(Debug)]
pub struct ReqwestHttpClient {
    client: reqwest::blocking::Client,
    #[cfg(test)]
    timeout: Duration,
}

impl ReqwestHttpClient {
    pub fn try_new() -> Result<Self> {
        let client = reqwest::blocking::Client::builder()
            .timeout(DEFAULT_HTTP_TIMEOUT)
            .build()
            .map_err(|source| FontbrewError::Network {
                message: format!("could not build HTTP client: {source}"),
            })?;

        Ok(Self {
            client,
            #[cfg(test)]
            timeout: DEFAULT_HTTP_TIMEOUT,
        })
    }
}

impl HttpClient for ReqwestHttpClient {
    fn get(&self, request: HttpRequest) -> Result<HttpResponse> {
        let mut builder = self.client.get(&request.url);
        for header in &request.headers {
            builder = builder.header(&header.name, &header.value);
        }

        let response = builder.send().map_err(|source| {
            let source = request_error_source(&request, source);
            FontbrewError::Network {
                message: format!("could not fetch {}: {source}", request.display_url()),
            }
        })?;
        let status = response.status().as_u16();
        let body = response.bytes().map_err(|source| {
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

    fn download_to_file(
        &self,
        request: HttpRequest,
        destination: &Path,
        max_bytes: u64,
        cancellation: &dyn CancellationToken,
    ) -> Result<u64> {
        ensure_not_cancelled(cancellation)?;

        let mut builder = self.client.get(&request.url);
        for header in &request.headers {
            builder = builder.header(&header.name, &header.value);
        }

        let mut response = builder.send().map_err(|source| {
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
            fs::create_dir_all(parent)?;
        }
        let mut destination_file = File::create(destination)?;
        let result = copy_limited_response(
            &mut response,
            &mut destination_file,
            max_bytes,
            request.display_url(),
            cancellation,
        );
        if result.is_err() {
            let _ = fs::remove_file(destination);
        }

        result
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

fn copy_limited_response(
    response: &mut impl Read,
    destination: &mut impl Write,
    max_bytes: u64,
    url: &str,
    cancellation: &dyn CancellationToken,
) -> Result<u64> {
    let mut downloaded = 0_u64;
    let mut buffer = [0_u8; DOWNLOAD_BUFFER_SIZE];

    loop {
        ensure_not_cancelled(cancellation)?;
        let read = response
            .read(&mut buffer)
            .map_err(|source| FontbrewError::Network {
                message: format!("could not read response body from {url}: {source}"),
            })?;
        if read == 0 {
            return Ok(downloaded);
        }

        ensure_not_cancelled(cancellation)?;
        let next_downloaded =
            downloaded
                .checked_add(read as u64)
                .ok_or_else(|| FontbrewError::ArchiveRejected {
                    reason: format!("download size overflowed for {url}"),
                })?;
        reject_oversized_download(next_downloaded, max_bytes, url)?;
        destination.write_all(&buffer[..read])?;
        downloaded = next_downloaded;
    }
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
    use std::time::Duration;

    use crate::Result;

    use super::{request_error_source, HttpRequest, ReqwestHttpClient};

    #[test]
    fn reqwest_client_try_new_uses_explicit_timeout() -> Result<()> {
        let client = ReqwestHttpClient::try_new()?;

        assert_eq!(client.timeout, Duration::from_secs(30));
        Ok(())
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
}
