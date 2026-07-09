mod fontsource;
pub(crate) mod github;

use crate::{FamilyName, FontFormat, PackageId, PackageVersion, ProviderKind};

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
    pub(crate) families: Vec<FamilyName>,
    pub(crate) assets: Vec<ProviderFontAsset>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProviderFontAsset {
    pub(crate) url: String,
    pub(crate) format: FontFormat,
    pub(crate) file_name: String,
    pub(crate) weight: Option<u16>,
}

pub(crate) use fontsource::{cached_fontsource_family, provider_asset_request, FontsourceProvider};
