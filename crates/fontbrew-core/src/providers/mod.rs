use std::{collections::BTreeMap, fs, path::PathBuf};

use serde::Deserialize;

use crate::{
    error::{FontbrewError, Result},
    fetch::{HttpClient, HttpHeader, HttpRequest},
    fs::write_atomically,
    model::{FamilyName, FontFormat, PackageVersion, SearchResult},
    platform::FontbrewPaths,
    PackageId,
};

const FONTSOURCE_API_BASE_URL: &str = "https://api.fontsource.org/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderSearchRequest<'a> {
    pub(crate) query: &'a str,
    pub(crate) limit: Option<usize>,
    pub(crate) offline: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FontsourceResolvedPackage {
    pub(crate) package_id: PackageId,
    pub(crate) provider_id: String,
    pub(crate) version: PackageVersion,
    pub(crate) assets: Vec<FontsourceFontAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FontsourceFontAsset {
    pub(crate) url: String,
    pub(crate) format: FontFormat,
    pub(crate) file_name: String,
}

#[derive(Clone, Copy)]
pub(crate) struct FontsourceProvider<'a> {
    paths: &'a FontbrewPaths,
    http_client: &'a dyn HttpClient,
}

impl<'a> FontsourceProvider<'a> {
    pub(crate) fn new(paths: &'a FontbrewPaths, http_client: &'a dyn HttpClient) -> Self {
        Self { paths, http_client }
    }

    pub(crate) fn search(&self, request: ProviderSearchRequest<'_>) -> Result<Vec<SearchResult>> {
        let query = request.query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }

        let snapshot_store = FontsourceSnapshotStore::new(self.paths);
        let list_records = if request.offline {
            match snapshot_store.read_list(query) {
                Ok(records) => records,
                Err(FontbrewError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Ok(Vec::new());
                }
                Err(error) => return Err(error),
            }
        } else {
            fetch_fontsource_list(self.http_client, &snapshot_store, query)?
        };

        let mut results = Vec::new();
        for record in list_records {
            if request.limit.is_some_and(|limit| results.len() >= limit) {
                break;
            }

            if PackageId::parse(&record.id).is_err() {
                continue;
            }

            let detail = if request.offline {
                match snapshot_store.read_detail(&record.id) {
                    Ok(detail) => detail,
                    Err(FontbrewError::Io(error))
                        if error.kind() == std::io::ErrorKind::NotFound =>
                    {
                        continue;
                    }
                    Err(error) => return Err(error),
                }
            } else {
                fetch_fontsource_detail(self.http_client, &snapshot_store, &record.id)?
            };

            let Some(result) = search_result_from_detail(&detail)? else {
                continue;
            };
            results.push(result);
        }

        Ok(results)
    }

    pub(crate) fn resolve_install_package(
        &self,
        provider_id: &str,
    ) -> Result<FontsourceResolvedPackage> {
        let package_id = PackageId::parse(provider_id)?;
        let snapshot_store = FontsourceSnapshotStore::new(self.paths);
        let detail = fetch_fontsource_detail(self.http_client, &snapshot_store, provider_id)?;

        if detail.id != provider_id {
            return Err(FontbrewError::ArchiveRejected {
                reason: format!(
                    "Fontsource detail id mismatch for {provider_id}: found {}",
                    detail.id
                ),
            });
        }

        let version = fontsource_version(&detail)?;
        let assets = desktop_assets(&detail);
        if assets.is_empty() {
            return Err(FontbrewError::ArchiveRejected {
                reason: format!(
                    "Fontsource package {provider_id} has no installable desktop font URLs"
                ),
            });
        }

        Ok(FontsourceResolvedPackage {
            package_id,
            provider_id: provider_id.to_string(),
            version,
            assets,
        })
    }
}

pub(crate) fn fontsource_asset_request(url: &str) -> HttpRequest {
    HttpRequest {
        url: url.to_string(),
        headers: fontsource_headers(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FontsourceListRecord {
    id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FontsourceDetailRecord {
    id: String,
    family: String,
    version: Option<String>,
    last_modified: Option<String>,
    #[serde(default)]
    variants: BTreeMap<String, BTreeMap<String, BTreeMap<String, FontsourceVariantRecord>>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct FontsourceVariantRecord {
    #[serde(default)]
    url: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
struct FontsourceSnapshotStore<'a> {
    paths: &'a FontbrewPaths,
}

impl<'a> FontsourceSnapshotStore<'a> {
    fn new(paths: &'a FontbrewPaths) -> Self {
        Self { paths }
    }

    fn read_list(&self, query: &str) -> Result<Vec<FontsourceListRecord>> {
        let body = fs::read(self.list_path(query))?;
        parse_fontsource_list(&body, query)
    }

    fn write_list(&self, query: &str, body: &[u8]) -> Result<()> {
        write_atomically(&self.list_path(query), body)
    }

    fn read_detail(&self, provider_id: &str) -> Result<FontsourceDetailRecord> {
        let body = fs::read(self.detail_path(provider_id))?;
        parse_fontsource_detail(&body, provider_id)
    }

    fn write_detail(&self, provider_id: &str, body: &[u8]) -> Result<()> {
        write_atomically(&self.detail_path(provider_id), body)
    }

    fn list_path(&self, query: &str) -> PathBuf {
        self.paths
            .provider_metadata_dir()
            .join(format!("fontsource-list-{}.json", hex_key(query)))
    }

    fn detail_path(&self, provider_id: &str) -> PathBuf {
        self.paths
            .provider_metadata_dir()
            .join(format!("fontsource-detail-{provider_id}.json"))
    }
}

fn fetch_fontsource_list(
    http_client: &dyn HttpClient,
    snapshot_store: &FontsourceSnapshotStore<'_>,
    query: &str,
) -> Result<Vec<FontsourceListRecord>> {
    let url = format!(
        "{FONTSOURCE_API_BASE_URL}/fonts?family={}",
        percent_encode(query)
    );
    let response = http_client.get(HttpRequest {
        url: url.clone(),
        headers: fontsource_headers(),
    })?;
    let body = successful_response_body(response.status, response.body, &url)?;
    let records = parse_fontsource_list(&body, query)?;
    snapshot_store.write_list(query, &body)?;

    Ok(records)
}

fn fetch_fontsource_detail(
    http_client: &dyn HttpClient,
    snapshot_store: &FontsourceSnapshotStore<'_>,
    provider_id: &str,
) -> Result<FontsourceDetailRecord> {
    let url = format!("{FONTSOURCE_API_BASE_URL}/fonts/{provider_id}");
    let response = http_client.get(HttpRequest {
        url: url.clone(),
        headers: fontsource_headers(),
    })?;
    let body = successful_response_body(response.status, response.body, &url)?;
    let detail = parse_fontsource_detail(&body, provider_id)?;
    snapshot_store.write_detail(provider_id, &body)?;

    Ok(detail)
}

fn parse_fontsource_list(body: &[u8], query: &str) -> Result<Vec<FontsourceListRecord>> {
    serde_json::from_slice(body).map_err(|source| FontbrewError::Network {
        message: format!("could not parse Fontsource search results for {query:?}: {source}"),
    })
}

fn parse_fontsource_detail(body: &[u8], provider_id: &str) -> Result<FontsourceDetailRecord> {
    serde_json::from_slice(body).map_err(|source| FontbrewError::Network {
        message: format!("could not parse Fontsource detail for {provider_id}: {source}"),
    })
}

fn search_result_from_detail(detail: &FontsourceDetailRecord) -> Result<Option<SearchResult>> {
    let package_id = match PackageId::parse(&detail.id) {
        Ok(package_id) => package_id,
        Err(_) => return Ok(None),
    };
    if desktop_assets(detail).is_empty() {
        return Ok(None);
    }

    Ok(Some(SearchResult {
        package_id,
        display_name: detail.family.clone(),
        source: format!("fontsource:{}", detail.id),
        version: fontsource_version(detail).ok(),
        families: vec![FamilyName::new(detail.family.clone())],
    }))
}

fn fontsource_version(detail: &FontsourceDetailRecord) -> Result<PackageVersion> {
    if let Some(version) = detail
        .version
        .as_ref()
        .filter(|version| !version.is_empty())
    {
        return PackageVersion::parse(version);
    }

    if let Some(last_modified) = detail
        .last_modified
        .as_ref()
        .filter(|last_modified| !last_modified.is_empty())
    {
        return PackageVersion::parse(last_modified);
    }

    Err(FontbrewError::ArchiveRejected {
        reason: format!(
            "Fontsource package {} has no version or lastModified metadata",
            detail.id
        ),
    })
}

fn desktop_assets(detail: &FontsourceDetailRecord) -> Vec<FontsourceFontAsset> {
    let mut assets = Vec::new();

    for (weight, styles) in &detail.variants {
        for (style, subsets) in styles {
            for (subset, variant) in subsets {
                for (url_key, format) in desktop_url_keys() {
                    let Some(url) = variant.url.get(url_key) else {
                        continue;
                    };

                    assets.push(FontsourceFontAsset {
                        url: url.clone(),
                        format,
                        file_name: format!(
                            "{}-{}-{}-{}.{}",
                            detail.id,
                            safe_file_component(subset),
                            safe_file_component(weight),
                            safe_file_component(style),
                            url_key
                        ),
                    });
                }
            }
        }
    }

    assets
}

fn desktop_url_keys() -> [(&'static str, FontFormat); 4] {
    [
        ("ttf", FontFormat::Ttf),
        ("otf", FontFormat::Otf),
        ("ttc", FontFormat::Ttc),
        ("otc", FontFormat::Otc),
    ]
}

fn safe_file_component(input: &str) -> String {
    let mut component = String::new();
    let mut previous_was_separator = false;

    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            component.push(character.to_ascii_lowercase());
            previous_was_separator = false;
            continue;
        }

        if matches!(character, '-' | '_' | '.') && !component.is_empty() && !previous_was_separator
        {
            component.push('-');
            previous_was_separator = true;
        }
    }

    while component.ends_with('-') {
        component.pop();
    }

    if component.is_empty() {
        "default".to_string()
    } else {
        component
    }
}

fn successful_response_body(status: u16, body: Vec<u8>, url: &str) -> Result<Vec<u8>> {
    if (200..300).contains(&status) {
        return Ok(body);
    }

    Err(FontbrewError::Network {
        message: format!("HTTP request failed with status {status} for {url}"),
    })
}

fn fontsource_headers() -> Vec<HttpHeader> {
    vec![
        HttpHeader {
            name: "User-Agent".to_string(),
            value: "fontbrew".to_string(),
        },
        HttpHeader {
            name: "Accept".to_string(),
            value: "application/json".to_string(),
        },
    ]
}

fn percent_encode(input: &str) -> String {
    let mut encoded = String::new();

    for byte in input.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }

    encoded
}

fn hex_key(input: &str) -> String {
    input
        .as_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join("")
}
