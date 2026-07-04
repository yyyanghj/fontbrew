use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

use serde::Deserialize;

use crate::{
    config::FontbrewConfig,
    config::GOOGLE_FONTS_API_KEY_ENV_VAR,
    error::{FontbrewError, Result},
    fetch::{HttpClient, HttpHeader, HttpRequest},
    fs::write_atomically,
    model::{FamilyName, FontFormat, PackageVersion, SearchResult},
    platform::FontbrewPaths,
    search::{best_search_match_score, SearchMatchScore},
    PackageId, ProviderKind,
};

const FONTSOURCE_API_BASE_URL: &str = "https://api.fontsource.org/v1";
const GOOGLE_FONTS_API_BASE_URL: &str = "https://www.googleapis.com/webfonts/v1/webfonts";
const DEFAULT_PROVIDER_SEARCH_LIMIT: usize = 25;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderSearchRequest<'a> {
    pub(crate) query: &'a str,
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedProviderPackage {
    pub(crate) package_id: PackageId,
    pub(crate) provider: ProviderKind,
    pub(crate) provider_id: String,
    pub(crate) version: PackageVersion,
    pub(crate) assets: Vec<ProviderFontAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderFontAsset {
    pub(crate) url: String,
    pub(crate) format: FontFormat,
    pub(crate) file_name: String,
}

pub(crate) type FontsourceResolvedPackage = ResolvedProviderPackage;
type FontsourceFontAsset = ProviderFontAsset;
pub(crate) type GoogleResolvedPackage = ResolvedProviderPackage;
type GoogleFontAsset = ProviderFontAsset;

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
        let raw_query = request.query.trim();
        if raw_query.is_empty() {
            return Ok(Vec::new());
        }
        let snapshot_store = FontsourceSnapshotStore::new(self.paths);
        let metadata_ttl = provider_metadata_ttl(self.paths)?;
        let list_records = fetch_fontsource_list(self.http_client, &snapshot_store, metadata_ttl)?;
        let mut matched_records = list_records
            .into_iter()
            .filter_map(|record| {
                fontsource_record_match_score(raw_query, &record).map(|score| (score, record))
            })
            .collect::<Vec<_>>();
        matched_records
            .sort_by(|left, right| left.0.cmp(&right.0).then(left.1.id.cmp(&right.1.id)));

        let mut results = Vec::new();
        let result_limit = provider_search_limit(request.limit);
        for (_, record) in matched_records {
            if results.len() >= result_limit {
                break;
            }

            if PackageId::parse(&record.id).is_err() {
                continue;
            }

            let detail = fetch_fontsource_detail(
                self.http_client,
                &snapshot_store,
                &record.id,
                metadata_ttl,
            )?;

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
        let metadata_ttl = provider_metadata_ttl(self.paths)?;
        let detail =
            fetch_fontsource_detail(self.http_client, &snapshot_store, provider_id, metadata_ttl)?;

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
            provider: ProviderKind::Fontsource,
            provider_id: provider_id.to_string(),
            version,
            assets,
        })
    }
}

pub(crate) fn provider_asset_request(url: &str) -> HttpRequest {
    HttpRequest {
        url: url.to_string(),
        display_url: None,
        headers: provider_asset_headers(),
    }
}

#[derive(Clone, Copy)]
pub(crate) struct GoogleProvider<'a> {
    paths: &'a FontbrewPaths,
    http_client: &'a dyn HttpClient,
}

impl<'a> GoogleProvider<'a> {
    pub(crate) fn new(paths: &'a FontbrewPaths, http_client: &'a dyn HttpClient) -> Self {
        Self { paths, http_client }
    }

    pub(crate) fn api_key_is_configured() -> bool {
        google_api_key_from_env().is_some()
    }

    pub(crate) fn search(&self, request: ProviderSearchRequest<'_>) -> Result<Vec<SearchResult>> {
        let raw_query = request.query.trim();
        if raw_query.is_empty() {
            return Ok(Vec::new());
        }
        let snapshot_store = GoogleSnapshotStore::new(self.paths);
        let response = fetch_google_webfonts(self.http_client, &snapshot_store, None)?;

        let mut matched_records = response
            .items
            .into_iter()
            .filter_map(|record| {
                let package_id = PackageId::normalize(&record.family).ok()?;
                let provider_id = package_id.as_str();
                let score =
                    best_search_match_score(raw_query, [provider_id, record.family.as_str()])?;
                Some((score, package_id, record))
            })
            .collect::<Vec<_>>();
        matched_records.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));

        let mut results = Vec::new();
        let result_limit = provider_search_limit(request.limit);
        for (_, package_id, record) in matched_records {
            if results.len() >= result_limit {
                break;
            }

            let provider_id = package_id.as_str().to_string();
            if google_desktop_assets(&record, &provider_id).is_empty() {
                continue;
            }

            results.push(SearchResult {
                package_id,
                display_name: record.family.clone(),
                source: format!("google:{provider_id}"),
                version: google_version(&record).ok(),
                families: vec![FamilyName::new(record.family)],
            });
        }

        Ok(results)
    }

    pub(crate) fn resolve_install_package(
        &self,
        provider_id: &str,
    ) -> Result<GoogleResolvedPackage> {
        let package_id = PackageId::parse(provider_id)?;
        let snapshot_store = GoogleSnapshotStore::new(self.paths);
        let family_query = provider_id_to_family_query(provider_id);
        let response = fetch_google_family(self.http_client, &snapshot_store, &family_query)?;
        let detail = response
            .items
            .into_iter()
            .find(|record| {
                PackageId::normalize(&record.family).is_ok_and(|record_id| record_id == package_id)
            })
            .ok_or_else(|| FontbrewError::ArchiveRejected {
                reason: format!("Google Fonts family {provider_id} was not found"),
            })?;

        let version = google_version(&detail)?;
        let assets = google_desktop_assets(&detail, provider_id);
        if assets.is_empty() {
            return Err(FontbrewError::ArchiveRejected {
                reason: format!(
                    "Google Fonts family {provider_id} has no installable desktop font URLs"
                ),
            });
        }

        Ok(GoogleResolvedPackage {
            package_id,
            provider: ProviderKind::Google,
            provider_id: provider_id.to_string(),
            version,
            assets,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FontsourceListRecord {
    id: String,
    family: String,
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

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
struct GoogleWebfontsResponse {
    #[serde(default)]
    items: Vec<GoogleFamilyRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GoogleFamilyRecord {
    family: String,
    version: Option<String>,
    last_modified: Option<String>,
    #[serde(default)]
    files: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy)]
struct FontsourceSnapshotStore<'a> {
    paths: &'a FontbrewPaths,
}

impl<'a> FontsourceSnapshotStore<'a> {
    fn new(paths: &'a FontbrewPaths) -> Self {
        Self { paths }
    }

    fn write_list(&self, body: &[u8]) -> Result<()> {
        write_atomically(&self.list_path(), body)
    }

    fn write_detail(&self, provider_id: &str, body: &[u8]) -> Result<()> {
        write_atomically(&self.detail_path(provider_id), body)
    }

    fn read_fresh_list(&self, metadata_ttl: Duration) -> Result<Option<Vec<u8>>> {
        read_fresh_snapshot(&self.list_path(), metadata_ttl)
    }

    fn read_list(&self) -> Result<Option<Vec<u8>>> {
        read_snapshot(&self.list_path())
    }

    fn read_fresh_detail(
        &self,
        provider_id: &str,
        metadata_ttl: Duration,
    ) -> Result<Option<Vec<u8>>> {
        read_fresh_snapshot(&self.detail_path(provider_id), metadata_ttl)
    }

    fn read_detail(&self, provider_id: &str) -> Result<Option<Vec<u8>>> {
        read_snapshot(&self.detail_path(provider_id))
    }

    fn list_path(&self) -> PathBuf {
        self.paths
            .provider_metadata_dir()
            .join("fontsource-list-all.json")
    }

    fn detail_path(&self, provider_id: &str) -> PathBuf {
        self.paths
            .provider_metadata_dir()
            .join(format!("fontsource-detail-{provider_id}.json"))
    }
}

#[derive(Debug, Clone, Copy)]
struct GoogleSnapshotStore<'a> {
    paths: &'a FontbrewPaths,
}

impl<'a> GoogleSnapshotStore<'a> {
    fn new(paths: &'a FontbrewPaths) -> Self {
        Self { paths }
    }

    fn write_family(&self, family_query: &str, body: &[u8]) -> Result<()> {
        write_atomically(&self.family_path(family_query), body)
    }

    fn family_path(&self, family_query: &str) -> PathBuf {
        self.paths
            .provider_metadata_dir()
            .join(format!("google-family-{}.json", hex_key(family_query)))
    }
}

fn fetch_fontsource_list(
    http_client: &dyn HttpClient,
    snapshot_store: &FontsourceSnapshotStore<'_>,
    metadata_ttl: Duration,
) -> Result<Vec<FontsourceListRecord>> {
    if let Some(body) = snapshot_store.read_fresh_list(metadata_ttl)? {
        if let Ok(records) = parse_fontsource_list(&body) {
            return Ok(records);
        }
    }

    let url = format!("{FONTSOURCE_API_BASE_URL}/fonts");
    let response = match http_client.get(HttpRequest {
        url: url.clone(),
        display_url: None,
        headers: fontsource_headers(),
    }) {
        Ok(response) => response,
        Err(error) => {
            return fontsource_cached_list_or_error(snapshot_store, error);
        }
    };
    let body = match successful_response_body(response.status, response.body, &url) {
        Ok(body) => body,
        Err(error) => return fontsource_cached_list_or_error(snapshot_store, error),
    };
    let records = parse_fontsource_list(&body)?;
    snapshot_store.write_list(&body)?;

    Ok(records)
}

fn fetch_fontsource_detail(
    http_client: &dyn HttpClient,
    snapshot_store: &FontsourceSnapshotStore<'_>,
    provider_id: &str,
    metadata_ttl: Duration,
) -> Result<FontsourceDetailRecord> {
    if let Some(body) = snapshot_store.read_fresh_detail(provider_id, metadata_ttl)? {
        if let Ok(detail) = parse_fontsource_detail(&body, provider_id) {
            return Ok(detail);
        }
    }

    let url = format!("{FONTSOURCE_API_BASE_URL}/fonts/{provider_id}");
    let response = match http_client.get(HttpRequest {
        url: url.clone(),
        display_url: None,
        headers: fontsource_headers(),
    }) {
        Ok(response) => response,
        Err(error) => {
            return fontsource_cached_detail_or_error(snapshot_store, provider_id, error);
        }
    };
    let body = match successful_response_body(response.status, response.body, &url) {
        Ok(body) => body,
        Err(error) => {
            return fontsource_cached_detail_or_error(snapshot_store, provider_id, error);
        }
    };
    let detail = parse_fontsource_detail(&body, provider_id)?;
    snapshot_store.write_detail(provider_id, &body)?;

    Ok(detail)
}

fn fontsource_cached_list_or_error(
    snapshot_store: &FontsourceSnapshotStore<'_>,
    error: FontbrewError,
) -> Result<Vec<FontsourceListRecord>> {
    if let Some(body) = snapshot_store.read_list()? {
        if let Ok(records) = parse_fontsource_list(&body) {
            return Ok(records);
        }
    }

    Err(error)
}

fn fontsource_cached_detail_or_error(
    snapshot_store: &FontsourceSnapshotStore<'_>,
    provider_id: &str,
    error: FontbrewError,
) -> Result<FontsourceDetailRecord> {
    if let Some(body) = snapshot_store.read_detail(provider_id)? {
        if let Ok(detail) = parse_fontsource_detail(&body, provider_id) {
            return Ok(detail);
        }
    }

    Err(error)
}

fn fetch_google_family(
    http_client: &dyn HttpClient,
    snapshot_store: &GoogleSnapshotStore<'_>,
    family_query: &str,
) -> Result<GoogleWebfontsResponse> {
    fetch_google_webfonts(http_client, snapshot_store, Some(family_query))
}

fn fetch_google_webfonts(
    http_client: &dyn HttpClient,
    snapshot_store: &GoogleSnapshotStore<'_>,
    family_query: Option<&str>,
) -> Result<GoogleWebfontsResponse> {
    let api_key = google_api_key_from_env_required()?;
    let query = match family_query {
        Some(family_query) => format!(
            "family={}&key={}",
            percent_encode(family_query),
            percent_encode(&api_key)
        ),
        None => format!("key={}", percent_encode(&api_key)),
    };
    let display_query = match family_query {
        Some(family_query) => format!("family={}&key=<redacted>", percent_encode(family_query)),
        None => "key=<redacted>".to_string(),
    };
    let url = format!("{GOOGLE_FONTS_API_BASE_URL}?{query}");
    let display_url = format!("{GOOGLE_FONTS_API_BASE_URL}?{display_query}");
    let response = http_client.get(HttpRequest {
        url,
        display_url: Some(display_url),
        headers: google_headers(),
    })?;
    let request_label = family_query.unwrap_or("all families");
    let body = successful_google_response_body(response.status, response.body, request_label)?;
    let response = parse_google_webfonts_response(&body, request_label)?;
    snapshot_store.write_family(request_label, &body)?;

    Ok(response)
}

fn parse_fontsource_list(body: &[u8]) -> Result<Vec<FontsourceListRecord>> {
    serde_json::from_slice(body).map_err(|source| FontbrewError::Network {
        message: format!("could not parse Fontsource search results: {source}"),
    })
}

fn parse_fontsource_detail(body: &[u8], provider_id: &str) -> Result<FontsourceDetailRecord> {
    serde_json::from_slice(body).map_err(|source| FontbrewError::Network {
        message: format!("could not parse Fontsource detail for {provider_id}: {source}"),
    })
}

fn parse_google_webfonts_response(
    body: &[u8],
    family_query: &str,
) -> Result<GoogleWebfontsResponse> {
    serde_json::from_slice(body).map_err(|source| FontbrewError::Network {
        message: format!("could not parse Google Fonts response for {family_query:?}: {source}"),
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

fn fontsource_record_match_score(
    query: &str,
    record: &FontsourceListRecord,
) -> Option<SearchMatchScore> {
    best_search_match_score(query, [record.id.as_str(), record.family.as_str()])
}

fn provider_search_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(DEFAULT_PROVIDER_SEARCH_LIMIT)
}

fn provider_metadata_ttl(paths: &FontbrewPaths) -> Result<Duration> {
    Ok(FontbrewConfig::load(&paths.config_path())?.metadata_ttl)
}

fn read_fresh_snapshot(path: &Path, metadata_ttl: Duration) -> Result<Option<Vec<u8>>> {
    if !snapshot_is_fresh(path, metadata_ttl)? {
        return Ok(None);
    }

    read_snapshot(path)
}

fn snapshot_is_fresh(path: &Path, metadata_ttl: Duration) -> Result<bool> {
    if metadata_ttl.is_zero() {
        return Ok(false);
    }

    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    let modified_at = metadata.modified()?;

    match SystemTime::now().duration_since(modified_at) {
        Ok(age) => Ok(age <= metadata_ttl),
        Err(_) => Ok(true),
    }
}

fn read_snapshot(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(body) => Ok(Some(body)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
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

fn google_version(detail: &GoogleFamilyRecord) -> Result<PackageVersion> {
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
            "Google Fonts family {} has no version or lastModified metadata",
            detail.family
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

fn google_desktop_assets(detail: &GoogleFamilyRecord, provider_id: &str) -> Vec<GoogleFontAsset> {
    let mut assets = Vec::new();

    for (variant, url) in &detail.files {
        let Some(format) = desktop_format_from_url(url) else {
            continue;
        };

        assets.push(GoogleFontAsset {
            url: url.clone(),
            format,
            file_name: format!(
                "{}-{}.{}",
                provider_id,
                safe_file_component(variant),
                font_format_extension(format)
            ),
        });
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

fn desktop_format_from_url(url: &str) -> Option<FontFormat> {
    let path = url
        .split(['?', '#'])
        .next()
        .unwrap_or(url)
        .to_ascii_lowercase();

    for (extension, format) in desktop_url_keys() {
        if path.ends_with(&format!(".{extension}")) {
            return Some(format);
        }
    }

    None
}

fn font_format_extension(format: FontFormat) -> &'static str {
    match format {
        FontFormat::Ttf => "ttf",
        FontFormat::Otf => "otf",
        FontFormat::Ttc => "ttc",
        FontFormat::Otc => "otc",
    }
}

fn provider_id_to_family_query(provider_id: &str) -> String {
    provider_id
        .split('-')
        .filter(|component| !component.is_empty())
        .map(title_case_ascii)
        .collect::<Vec<_>>()
        .join(" ")
}

fn title_case_ascii(component: &str) -> String {
    let mut characters = component.chars();
    let Some(first) = characters.next() else {
        return String::new();
    };

    let mut title = String::new();
    title.push(first.to_ascii_uppercase());
    title.extend(characters.map(|character| character.to_ascii_lowercase()));
    title
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

fn successful_google_response_body(
    status: u16,
    body: Vec<u8>,
    family_query: &str,
) -> Result<Vec<u8>> {
    if (200..300).contains(&status) {
        return Ok(body);
    }

    if status == 429 {
        return Err(FontbrewError::Network {
            message: format!(
                "Google Fonts rate limit returned HTTP 429 for family {family_query:?}; retry later or use a Google Fonts API key with available quota in {GOOGLE_FONTS_API_KEY_ENV_VAR}"
            ),
        });
    }

    Err(FontbrewError::Network {
        message: format!(
            "Google Fonts API request failed with status {status} for family {family_query:?}; check {GOOGLE_FONTS_API_KEY_ENV_VAR} and Google Fonts API access"
        ),
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

fn google_headers() -> Vec<HttpHeader> {
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

fn provider_asset_headers() -> Vec<HttpHeader> {
    vec![HttpHeader {
        name: "User-Agent".to_string(),
        value: "fontbrew".to_string(),
    }]
}

fn google_api_key_from_env_required() -> Result<String> {
    google_api_key_from_env().ok_or_else(|| FontbrewError::Config {
        message: format!(
            "Google Fonts API requires {GOOGLE_FONTS_API_KEY_ENV_VAR}; set it in the environment before searching or installing google:<id> sources"
        ),
    })
}

fn google_api_key_from_env() -> Option<String> {
    std::env::var(GOOGLE_FONTS_API_KEY_ENV_VAR)
        .ok()
        .map(|api_key| api_key.trim().to_string())
        .filter(|api_key| !api_key.is_empty())
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
