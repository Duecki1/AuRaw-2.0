use crate::pipeline::RawThumbnail;
use ring::digest::{Context as Sha256Context, SHA256};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MAX_CATALOG_BYTES: u64 = 16 * 1024 * 1024;
const MAX_CATALOG_ASSETS: usize = 20_000;
const MAX_THUMBNAIL_BYTES: u64 = 32 * 1024 * 1024;
const MAX_RAW_BYTES: u64 = 16 * 1024 * 1024 * 1024;
const MAX_METADATA_BYTES: u64 = 64 * 1024;
const MAX_UPLOAD_FILENAME_BYTES: usize = 1024;
pub const CLOUD_ROOT_FOLDER_ID: &str = "root";

fn default_cloud_folder_id() -> String {
    CLOUD_ROOT_FOLDER_ID.to_owned()
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CloudConfig {
    pub enabled: bool,
    pub server_url: String,
    pub access_token: String,
}

impl CloudConfig {
    pub fn normalized(&self) -> Result<Self, String> {
        let mut server_url = self.server_url.trim().trim_end_matches('/').to_owned();
        if server_url.is_empty() {
            return Err("Enter the AuRaw Cloud server address.".to_owned());
        }
        if !server_url.contains("://") {
            server_url = format!("http://{server_url}");
        }
        if !server_url.starts_with("http://") && !server_url.starts_with("https://") {
            return Err("The cloud address must use http:// or https://.".to_owned());
        }
        if server_url[server_url.find("://").unwrap_or_default() + 3..].is_empty() {
            return Err("The cloud address is missing a host name or IP address.".to_owned());
        }
        Ok(Self {
            enabled: self.enabled,
            server_url,
            access_token: self.access_token.trim().to_owned(),
        })
    }

    fn endpoint(&self, suffix: &str) -> Result<String, String> {
        let normalized = self.normalized()?;
        Ok(format!("{}{suffix}", normalized.server_url))
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CloudThumbnailKind {
    Raw,
    Edited,
    Placeholder,
    #[default]
    Legacy,
}

impl CloudThumbnailKind {
    pub fn is_unedited(self) -> bool {
        !matches!(self, Self::Edited)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct CloudAsset {
    pub id: String,
    pub name: String,
    pub bytes: u64,
    pub modified_seconds: u64,
    pub width: u32,
    pub height: u32,
    pub raw_etag: String,
    pub sidecar_etag: Option<String>,
    pub thumbnail_etag: String,
    #[serde(default)]
    pub thumbnail_kind: CloudThumbnailKind,
    #[serde(default = "default_cloud_folder_id")]
    pub folder_id: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct CloudFolder {
    pub id: String,
    pub parent_id: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
pub struct CloudTrashItem {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub deleted_seconds: u64,
    pub expires_seconds: u64,
    pub bytes: u64,
    pub item_count: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CloudTrashCatalog {
    pub items: Vec<CloudTrashItem>,
    pub server_time: u64,
    pub retention_days: u32,
}

#[derive(Deserialize, Serialize)]
struct CloudCatalog {
    items: Vec<CloudAsset>,
    #[serde(default)]
    folders: Vec<CloudFolder>,
}

#[derive(Deserialize, Serialize)]
struct CachedCloudCatalog {
    schema_version: u32,
    server_url: String,
    items: Vec<CloudAsset>,
    #[serde(default)]
    folders: Vec<CloudFolder>,
}

#[derive(Clone, Debug)]
pub struct CloudCatalogSnapshot {
    pub items: Vec<CloudAsset>,
    pub folders: Vec<CloudFolder>,
    /// Set when `items` came from the last successful refresh rather than the
    /// server. The text is suitable for the library status line.
    pub offline_reason: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CachedAssetMetadata {
    schema_version: u32,
    asset_id: String,
    server_url: String,
    access_token: String,
    raw_etag: String,
    sidecar_etag: Option<String>,
    thumbnail_etag: String,
    pending_sidecar_upload: bool,
    #[serde(default)]
    sync_issue: Option<CachedSyncIssue>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum CachedSyncIssue {
    Failed,
    Conflict,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CloudSyncState {
    #[default]
    Synced,
    Queued,
    Failed,
    Conflict,
}

#[derive(Clone, Debug)]
pub struct CachedCloudAsset {
    pub asset_id: String,
    pub raw_path: PathBuf,
    pub label: String,
    pub offline_reason: Option<String>,
}

enum CatalogFetchError {
    Unavailable(String),
    Fatal(String),
}

impl CatalogFetchError {
    fn message(self) -> String {
        match self {
            Self::Unavailable(message) | Self::Fatal(message) => message,
        }
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(20)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .timeout_recv_body(Some(Duration::from_secs(60 * 60)))
        .build()
        .into()
}

fn authorization(config: &CloudConfig) -> Option<String> {
    let token = config.access_token.trim();
    (!token.is_empty()).then(|| format!("Bearer {token}"))
}

fn get(config: &CloudConfig, url: &str) -> Result<ureq::http::Response<ureq::Body>, ureq::Error> {
    let request = agent().get(url);
    if let Some(value) = authorization(config) {
        request.header("Authorization", value).call()
    } else {
        request.call()
    }
}

fn validate_hex_identifier(value: &str, label: &str) -> Result<(), String> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("Cloud returned an invalid {label}."));
    }
    Ok(())
}

fn validate_folder_identifier(value: &str, label: &str) -> Result<(), String> {
    if value == CLOUD_ROOT_FOLDER_ID {
        Ok(())
    } else {
        validate_hex_identifier(value, label)
    }
}

fn validate_folder_name(value: &str) -> Result<(), String> {
    let name = Path::new(value);
    if value.is_empty()
        || value.contains(['/', '\\'])
        || value.contains('"')
        || value.chars().any(char::is_control)
        || value.len() > 255
        || name.file_name().and_then(|name| name.to_str()) != Some(value)
    {
        return Err("Cloud returned an unsafe folder name.".to_owned());
    }
    Ok(())
}

fn validate_asset(asset: &CloudAsset) -> Result<(), String> {
    validate_hex_identifier(&asset.id, "asset ID")?;
    validate_hex_identifier(&asset.raw_etag, "RAW version")?;
    validate_hex_identifier(&asset.thumbnail_etag, "thumbnail version")?;
    if let Some(etag) = &asset.sidecar_etag {
        validate_hex_identifier(etag, "sidecar version")?;
    }
    validate_folder_identifier(&asset.folder_id, "asset folder ID")?;
    let name = Path::new(&asset.name);
    if asset.name.is_empty()
        || name.file_name().and_then(|name| name.to_str()) != Some(asset.name.as_str())
    {
        return Err("Cloud returned an unsafe RAW filename.".to_owned());
    }
    if asset.bytes == 0 || asset.bytes > MAX_RAW_BYTES {
        return Err(format!(
            "Cloud RAW {} has an invalid size of {} bytes.",
            asset.name, asset.bytes
        ));
    }
    if asset.width == 0 || asset.height == 0 || asset.width > 32_768 || asset.height > 32_768 {
        return Err(format!(
            "Cloud thumbnail metadata for {} has invalid dimensions.",
            asset.name
        ));
    }
    Ok(())
}

fn validate_catalog(catalog: &CloudCatalog) -> Result<(), String> {
    if catalog.items.len() > MAX_CATALOG_ASSETS {
        return Err(format!(
            "Cloud returned more than {MAX_CATALOG_ASSETS} catalog entries."
        ));
    }
    if catalog.folders.len() > MAX_CATALOG_ASSETS {
        return Err(format!(
            "Cloud returned more than {MAX_CATALOG_ASSETS} folders."
        ));
    }

    let mut folder_parents = HashMap::with_capacity(catalog.folders.len());
    for folder in &catalog.folders {
        validate_hex_identifier(&folder.id, "folder ID")?;
        validate_folder_identifier(&folder.parent_id, "parent folder ID")?;
        validate_folder_name(&folder.name)?;
        if folder_parents
            .insert(folder.id.as_str(), folder.parent_id.as_str())
            .is_some()
        {
            return Err("Cloud returned duplicate folder IDs.".to_owned());
        }
    }
    for folder in &catalog.folders {
        if folder.parent_id != CLOUD_ROOT_FOLDER_ID
            && !folder_parents.contains_key(folder.parent_id.as_str())
        {
            return Err("Cloud returned an invalid folder hierarchy.".to_owned());
        }
    }

    // Each folder has one parent. Remember already-rooted paths so even a very
    // deep catalog is validated in linear time rather than walking the same
    // ancestor chain once per folder.
    let mut rooted = HashSet::with_capacity(catalog.folders.len());
    for folder in &catalog.folders {
        let mut current = folder.id.as_str();
        let mut path = Vec::new();
        let mut visiting = HashSet::new();
        while current != CLOUD_ROOT_FOLDER_ID && !rooted.contains(current) {
            if !visiting.insert(current) {
                return Err("Cloud returned a cyclic folder hierarchy.".to_owned());
            }
            path.push(current);
            let Some(parent) = folder_parents.get(current) else {
                return Err("Cloud returned an invalid folder hierarchy.".to_owned());
            };
            current = parent;
        }
        rooted.extend(path);
    }
    for asset in &catalog.items {
        validate_asset(asset)?;
        if asset.folder_id != CLOUD_ROOT_FOLDER_ID
            && !folder_parents.contains_key(asset.folder_id.as_str())
        {
            return Err("Cloud returned a RAW in an unknown folder.".to_owned());
        }
    }
    Ok(())
}

fn validate_trash_item(item: &CloudTrashItem) -> Result<(), String> {
    validate_hex_identifier(&item.id, "Trash item ID")?;
    if !matches!(item.kind.as_str(), "asset" | "folder") {
        return Err("Cloud returned an invalid Trash item kind.".to_owned());
    }
    if item.kind == "asset" {
        validate_upload_name(&item.name)?;
    } else {
        validate_folder_name(&item.name)?;
    }
    if item.expires_seconds < item.deleted_seconds || item.item_count == 0 {
        return Err("Cloud returned invalid Trash retention metadata.".to_owned());
    }
    Ok(())
}

fn catalog_request_error(error: ureq::Error) -> CatalogFetchError {
    let message = match error {
        ureq::Error::StatusCode(401) => {
            return CatalogFetchError::Fatal("AuRaw Cloud rejected the access token.".to_owned());
        }
        ureq::Error::StatusCode(status) if status >= 500 || status == 408 || status == 429 => {
            format!("AuRaw Cloud is temporarily unavailable (HTTP status {status}).")
        }
        ureq::Error::StatusCode(status) => {
            return CatalogFetchError::Fatal(format!(
                "AuRaw Cloud returned HTTP status {status} while loading the catalog."
            ));
        }
        ureq::Error::Io(_)
        | ureq::Error::Timeout(_)
        | ureq::Error::HostNotFound
        | ureq::Error::ConnectionFailed => format!("Could not reach AuRaw Cloud: {error}"),
        _ => {
            return CatalogFetchError::Fatal(format!("Could not load the cloud catalog: {error}"));
        }
    };
    CatalogFetchError::Unavailable(message)
}

fn fetch_catalog(config: &CloudConfig) -> Result<CloudCatalog, CatalogFetchError> {
    let normalized = config.normalized().map_err(CatalogFetchError::Fatal)?;
    let url = normalized
        .endpoint("/api/v1/assets")
        .map_err(CatalogFetchError::Fatal)?;
    let mut response = get(&normalized, &url).map_err(catalog_request_error)?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_CATALOG_BYTES)
        .read_to_vec()
        .map_err(|error| {
            CatalogFetchError::Unavailable(format!("Could not read the cloud catalog: {error}"))
        })?;
    let catalog: CloudCatalog = serde_json::from_slice(&bytes).map_err(|error| {
        CatalogFetchError::Fatal(format!("Cloud returned an invalid catalog: {error}"))
    })?;
    validate_catalog(&catalog).map_err(CatalogFetchError::Fatal)?;
    Ok(catalog)
}

pub fn list_assets(config: &CloudConfig) -> Result<Vec<CloudAsset>, String> {
    fetch_catalog(config)
        .map(|catalog| catalog.items)
        .map_err(CatalogFetchError::message)
}

fn catalog_cache_path(cache_root: &Path, config: &CloudConfig) -> Result<PathBuf, String> {
    let config = config.normalized()?;
    let mut digest = Sha256Context::new(&SHA256);
    digest.update(config.server_url.as_bytes());
    digest.update(&[0]);
    digest.update(config.access_token.as_bytes());
    Ok(cache_root
        .join("catalogs")
        .join(format!("{}.json", sha256_hex(digest))))
}

fn save_catalog_cache(
    config: &CloudConfig,
    cache_root: &Path,
    catalog: &CloudCatalog,
) -> Result<(), String> {
    let normalized = config.normalized()?;
    let catalog = CachedCloudCatalog {
        schema_version: 2,
        server_url: normalized.server_url.clone(),
        items: catalog.items.clone(),
        folders: catalog.folders.clone(),
    };
    let bytes = serde_json::to_vec(&catalog)
        .map_err(|error| format!("Could not encode the cloud catalog cache: {error}"))?;
    if bytes.len() as u64 > MAX_CATALOG_BYTES {
        return Err("The cloud catalog cache exceeds its safety limit.".to_owned());
    }
    let path = catalog_cache_path(cache_root, &normalized)?;
    crate::thumbnail_cache::write_bytes_atomic(&path, &bytes)
        .map_err(|error| format!("Could not save the cloud catalog cache: {error}"))
}

fn load_catalog_cache(config: &CloudConfig, cache_root: &Path) -> Result<CloudCatalog, String> {
    let normalized = config.normalized()?;
    let path = catalog_cache_path(cache_root, &normalized)?;
    let metadata = fs::metadata(&path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            "No cached cloud library is available yet. Connect once to make it available offline."
                .to_owned()
        } else {
            format!("Could not inspect the cached cloud library: {error}")
        }
    })?;
    if !metadata.is_file() || metadata.len() > MAX_CATALOG_BYTES {
        return Err("The cached cloud library exceeds its safety limit.".to_owned());
    }
    let bytes = fs::read(&path)
        .map_err(|error| format!("Could not read the cached cloud library: {error}"))?;
    let catalog: CachedCloudCatalog = serde_json::from_slice(&bytes)
        .map_err(|error| format!("The cached cloud library is invalid: {error}"))?;
    if !(1..=2).contains(&catalog.schema_version) || catalog.server_url != normalized.server_url {
        return Err("The cached cloud library belongs to a different server version.".to_owned());
    }
    let catalog = CloudCatalog {
        items: catalog.items,
        folders: catalog.folders,
    };
    validate_catalog(&catalog)
        .map_err(|error| format!("The cached cloud library is invalid: {error}"))?;
    Ok(catalog)
}

/// Loads the current catalog and remembers it for offline browsing. Only
/// transport failures fall back to cache; authentication and malformed server
/// responses remain visible rather than silently exposing stale data.
pub fn list_assets_cached(
    config: &CloudConfig,
    cache_root: &Path,
    allow_network: bool,
) -> Result<CloudCatalogSnapshot, String> {
    if allow_network {
        match fetch_catalog(config) {
            Ok(catalog) => {
                if let Err(error) = save_catalog_cache(config, cache_root, &catalog) {
                    log::warn!("{error}");
                }
                return Ok(CloudCatalogSnapshot {
                    items: catalog.items,
                    folders: catalog.folders,
                    offline_reason: None,
                });
            }
            Err(CatalogFetchError::Fatal(error)) => return Err(error),
            Err(CatalogFetchError::Unavailable(error)) => {
                let catalog = load_catalog_cache(config, cache_root)
                    .map_err(|cache_error| format!("{error} {cache_error}"))?;
                return Ok(CloudCatalogSnapshot {
                    items: catalog.items,
                    folders: catalog.folders,
                    offline_reason: Some(error),
                });
            }
        }
    }

    let catalog = load_catalog_cache(config, cache_root)?;
    Ok(CloudCatalogSnapshot {
        items: catalog.items,
        folders: catalog.folders,
        offline_reason: Some("Android is offline; showing the cached cloud library.".to_owned()),
    })
}

pub fn test_connection(config: &CloudConfig) -> Result<String, String> {
    let assets = list_assets(config)?;
    Ok(format!(
        "Connected to AuRaw Cloud · {} {}",
        assets.len(),
        if assets.len() == 1 { "photo" } else { "photos" }
    ))
}

#[derive(Serialize)]
struct FolderMutation<'a> {
    parent_id: &'a str,
    name: &'a str,
}

#[derive(Serialize)]
struct AssetMutation<'a> {
    folder_id: &'a str,
    name: &'a str,
}

#[derive(Serialize)]
struct CloudDestination<'a> {
    folder_id: &'a str,
}

fn mutation_error(error: ureq::Error, action: &str) -> String {
    match error {
        ureq::Error::StatusCode(400) => {
            format!("AuRaw Cloud rejected the requested {action}.")
        }
        ureq::Error::StatusCode(401) => "AuRaw Cloud rejected the access token.".to_owned(),
        ureq::Error::StatusCode(404) => format!(
            "The cloud destination no longer exists, or the companion server has not been rebuilt with support for {action}. Update and restart AuRaw Cloud, then refresh."
        ),
        ureq::Error::StatusCode(409) => {
            format!("AuRaw Cloud could not {action} because that name or location conflicts.")
        }
        ureq::Error::StatusCode(412) => {
            "The cloud edit changed on another client. Refresh and try again.".to_owned()
        }
        _ => format!("Could not {action} in AuRaw Cloud: {error}"),
    }
}

fn post_json<T: Serialize>(
    config: &CloudConfig,
    suffix: &str,
    value: &T,
    action: &str,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let config = config.normalized()?;
    let url = config.endpoint(suffix)?;
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("Could not encode cloud {action}: {error}"))?;
    let request = agent().post(&url).header("Content-Type", "application/json");
    let request = if let Some(value) = authorization(&config) {
        request.header("Authorization", value)
    } else {
        request
    };
    request.send(&bytes).map_err(|error| mutation_error(error, action))
}

fn patch_json<T: Serialize>(
    config: &CloudConfig,
    suffix: &str,
    value: &T,
    action: &str,
) -> Result<ureq::http::Response<ureq::Body>, String> {
    let config = config.normalized()?;
    let url = config.endpoint(suffix)?;
    let bytes = serde_json::to_vec(value)
        .map_err(|error| format!("Could not encode cloud {action}: {error}"))?;
    let request = agent().patch(&url).header("Content-Type", "application/json");
    let request = if let Some(value) = authorization(&config) {
        request.header("Authorization", value)
    } else {
        request
    };
    request.send(&bytes).map_err(|error| mutation_error(error, action))
}

fn delete_request(
    config: &CloudConfig,
    suffix: &str,
    etag: Option<&str>,
    action: &str,
) -> Result<(), String> {
    let config = config.normalized()?;
    let url = config.endpoint(suffix)?;
    let request = agent().delete(&url);
    let request = if let Some(value) = authorization(&config) {
        request.header("Authorization", value)
    } else {
        request
    };
    let request = if let Some(etag) = etag {
        request.header("If-Match", format!("\"{etag}\""))
    } else {
        request
    };
    request
        .call()
        .map(|_| ())
        .map_err(|error| mutation_error(error, action))
}

fn decode_mutation_response<T: for<'de> Deserialize<'de>>(
    mut response: ureq::http::Response<ureq::Body>,
    label: &str,
) -> Result<T, String> {
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_METADATA_BYTES)
        .read_to_vec()
        .map_err(|error| format!("Could not read the cloud {label} response: {error}"))?;
    serde_json::from_slice(&bytes)
        .map_err(|error| format!("Cloud returned an invalid {label} response: {error}"))
}

pub fn create_folder(
    config: &CloudConfig,
    parent_id: &str,
    name: &str,
) -> Result<CloudFolder, String> {
    validate_folder_identifier(parent_id, "parent folder ID")?;
    validate_folder_name(name).map_err(|_| "Enter a valid folder name.".to_owned())?;
    let response = post_json(
        config,
        "/api/v1/folders",
        &FolderMutation { parent_id, name },
        "create a folder",
    )?;
    let folder: CloudFolder = decode_mutation_response(response, "folder")?;
    validate_hex_identifier(&folder.id, "folder ID")?;
    validate_folder_identifier(&folder.parent_id, "parent folder ID")?;
    validate_folder_name(&folder.name)?;
    Ok(folder)
}

pub fn update_folder(
    config: &CloudConfig,
    folder: &CloudFolder,
    parent_id: &str,
    name: &str,
) -> Result<CloudFolder, String> {
    validate_hex_identifier(&folder.id, "folder ID")?;
    validate_folder_identifier(parent_id, "parent folder ID")?;
    validate_folder_name(name).map_err(|_| "Enter a valid folder name.".to_owned())?;
    let response = patch_json(
        config,
        &format!("/api/v1/folders/{}", folder.id),
        &FolderMutation { parent_id, name },
        "update the folder",
    )?;
    let updated: CloudFolder = decode_mutation_response(response, "folder")?;
    validate_hex_identifier(&updated.id, "folder ID")?;
    validate_folder_identifier(&updated.parent_id, "parent folder ID")?;
    validate_folder_name(&updated.name)?;
    Ok(updated)
}

pub fn copy_folder(
    config: &CloudConfig,
    folder: &CloudFolder,
    destination_parent_id: &str,
) -> Result<CloudFolder, String> {
    validate_hex_identifier(&folder.id, "folder ID")?;
    validate_folder_identifier(destination_parent_id, "destination folder ID")?;
    let response = post_json(
        config,
        &format!("/api/v1/folders/{}/copy", folder.id),
        &CloudDestination {
            folder_id: destination_parent_id,
        },
        "copy the folder",
    )?;
    let copied: CloudFolder = decode_mutation_response(response, "folder")?;
    validate_hex_identifier(&copied.id, "folder ID")?;
    validate_folder_identifier(&copied.parent_id, "parent folder ID")?;
    validate_folder_name(&copied.name)?;
    Ok(copied)
}

pub fn delete_folder(config: &CloudConfig, folder_id: &str) -> Result<(), String> {
    validate_hex_identifier(folder_id, "folder ID")?;
    delete_request(
        config,
        &format!("/api/v1/folders/{folder_id}"),
        None,
        "delete the folder",
    )
}

pub fn update_asset(
    config: &CloudConfig,
    asset: &CloudAsset,
    folder_id: &str,
    name: &str,
) -> Result<CloudAsset, String> {
    validate_asset(asset)?;
    validate_folder_identifier(folder_id, "destination folder ID")?;
    validate_upload_name(name)?;
    let response = patch_json(
        config,
        &format!("/api/v1/assets/{}", asset.id),
        &AssetMutation { folder_id, name },
        "update the RAW",
    )?;
    let updated: CloudAsset = decode_mutation_response(response, "RAW")?;
    validate_asset(&updated)?;
    Ok(updated)
}

pub fn copy_asset(
    config: &CloudConfig,
    asset: &CloudAsset,
    destination_folder_id: &str,
) -> Result<CloudAsset, String> {
    validate_asset(asset)?;
    validate_folder_identifier(destination_folder_id, "destination folder ID")?;
    let response = post_json(
        config,
        &format!("/api/v1/assets/{}/copy", asset.id),
        &CloudDestination {
            folder_id: destination_folder_id,
        },
        "copy the RAW",
    )?;
    let copied: CloudAsset = decode_mutation_response(response, "RAW")?;
    validate_asset(&copied)?;
    Ok(copied)
}

pub fn delete_asset(config: &CloudConfig, asset: &CloudAsset) -> Result<(), String> {
    validate_asset(asset)?;
    delete_request(
        config,
        &format!("/api/v1/assets/{}", asset.id),
        None,
        "delete the RAW",
    )
}

#[derive(Serialize)]
struct TrashRestoreDestination<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    folder_id: Option<&'a str>,
}

#[derive(Deserialize)]
struct TrashRestoreResponse {
    kind: String,
    name: String,
}

pub fn list_trash(config: &CloudConfig) -> Result<CloudTrashCatalog, String> {
    let normalized = config.normalized()?;
    let url = normalized.endpoint("/api/v1/trash")?;
    let mut response = get(&normalized, &url).map_err(|error| mutation_error(error, "load Trash"))?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_CATALOG_BYTES)
        .read_to_vec()
        .map_err(|error| format!("Could not read AuRaw Cloud Trash: {error}"))?;
    let catalog: CloudTrashCatalog = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Cloud returned invalid Trash metadata: {error}"))?;
    if catalog.items.len() > MAX_CATALOG_ASSETS || catalog.retention_days == 0 {
        return Err("Cloud returned invalid Trash catalog limits.".to_owned());
    }
    for item in &catalog.items {
        validate_trash_item(item)?;
    }
    Ok(catalog)
}

pub fn restore_trash_item(
    config: &CloudConfig,
    item: &CloudTrashItem,
    folder_id: Option<&str>,
) -> Result<String, String> {
    validate_trash_item(item)?;
    if let Some(folder_id) = folder_id {
        validate_folder_identifier(folder_id, "restore folder ID")?;
    }
    let response = post_json(
        config,
        &format!("/api/v1/trash/{}/restore", item.id),
        &TrashRestoreDestination { folder_id },
        "restore the Trash item",
    )?;
    let restored: TrashRestoreResponse = decode_mutation_response(response, "Trash restore")?;
    if restored.kind != item.kind {
        return Err("Cloud returned a mismatched Trash restore result.".to_owned());
    }
    if restored.kind == "asset" {
        validate_upload_name(&restored.name)?;
    } else {
        validate_folder_name(&restored.name)?;
    }
    Ok(restored.name)
}

pub fn permanently_delete_trash_item(
    config: &CloudConfig,
    item: &CloudTrashItem,
) -> Result<(), String> {
    validate_trash_item(item)?;
    delete_request(
        config,
        &format!("/api/v1/trash/{}", item.id),
        None,
        "permanently delete the Trash item",
    )
}

pub fn empty_trash(config: &CloudConfig) -> Result<(), String> {
    delete_request(config, "/api/v1/trash", None, "empty Trash")
}

pub fn reset_asset_sidecar(config: &CloudConfig, asset: &CloudAsset) -> Result<(), String> {
    validate_asset(asset)?;
    let Some(etag) = asset.sidecar_etag.as_deref() else {
        return Ok(());
    };
    delete_request(
        config,
        &format!("/api/v1/assets/{}/sidecar", asset.id),
        Some(etag),
        "reset the RAW adjustments",
    )
}

fn validate_upload_name(display_name: &str) -> Result<(), String> {
    let name = Path::new(display_name);
    if display_name.is_empty()
        || display_name.contains(['/', '\\'])
        || display_name.contains('"')
        || display_name.chars().any(char::is_control)
        || display_name.len() > MAX_UPLOAD_FILENAME_BYTES
        || name.file_name().and_then(|name| name.to_str()) != Some(display_name)
        || !crate::pipeline::is_supported_raw_path(name)
    {
        return Err(format!("{display_name:?} is not a supported RAW filename."));
    }
    Ok(())
}

fn validate_upload_size(display_name: &str, bytes: Option<u64>) -> Result<(), String> {
    if bytes == Some(0) {
        return Err(format!("{display_name} is empty."));
    }
    if bytes.is_some_and(|bytes| bytes > MAX_RAW_BYTES) {
        return Err(format!(
            "{display_name} exceeds AuRaw Cloud's 16 GiB client limit."
        ));
    }
    Ok(())
}

fn checked_upload_part<'a>(
    path: &Path,
    maximum_bytes: u64,
    label: &str,
) -> Result<ureq::unversioned::multipart::Part<'a>, String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("Could not inspect {label} {}: {error}", path.display()))?;
    if !metadata.is_file() || metadata.len() > maximum_bytes {
        return Err(format!(
            "{label} {} exceeds its upload safety limit.",
            path.display()
        ));
    }
    ureq::unversioned::multipart::Part::file(path)
        .map_err(|error| format!("Could not open {label} {}: {error}", path.display()))
}

fn send_upload_form<'a>(
    config: &CloudConfig,
    form: ureq::unversioned::multipart::Form<'a>,
    folder_id: &'a str,
) -> Result<CloudAsset, String> {
    validate_folder_identifier(folder_id, "upload folder ID")?;
    let config = config.normalized()?;
    let url = config.endpoint("/api/v1/assets")?;
    let request = agent().post(&url);
    let request = if let Some(value) = authorization(&config) {
        request.header("Authorization", value)
    } else {
        request
    };
    let form = form.text("folder_id", folder_id);
    let mut response = request.send(form).map_err(|error| match error {
        ureq::Error::StatusCode(400) => {
            "AuRaw Cloud rejected this RAW or its filename.".to_owned()
        }
        ureq::Error::StatusCode(401) => "AuRaw Cloud rejected the access token.".to_owned(),
        ureq::Error::StatusCode(413) => {
            "AuRaw Cloud rejected an upload because it is too large.".to_owned()
        }
        _ => format!("Could not upload to AuRaw Cloud: {error}"),
    })?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_METADATA_BYTES)
        .read_to_vec()
        .map_err(|error| format!("Could not read the cloud upload response: {error}"))?;
    let asset: CloudAsset = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Cloud returned an invalid upload response: {error}"))?;
    validate_asset(&asset)?;
    Ok(asset)
}

#[cfg(not(target_os = "android"))]
pub fn upload_asset_path(config: &CloudConfig, raw_path: &Path) -> Result<CloudAsset, String> {
    upload_asset_path_to_folder(config, raw_path, CLOUD_ROOT_FOLDER_ID)
}

#[cfg(not(target_os = "android"))]
pub fn upload_asset_path_to_folder(
    config: &CloudConfig,
    raw_path: &Path,
    folder_id: &str,
) -> Result<CloudAsset, String> {
    let display_name = raw_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| "The selected RAW filename is not valid UTF-8.".to_owned())?;
    validate_upload_name(display_name)?;
    let raw_metadata = fs::metadata(raw_path)
        .map_err(|error| format!("Could not inspect {}: {error}", raw_path.display()))?;
    if !raw_metadata.is_file() {
        return Err(format!("{} is not a file.", raw_path.display()));
    }
    validate_upload_size(display_name, Some(raw_metadata.len()))?;

    let raw = checked_upload_part(raw_path, MAX_RAW_BYTES, "RAW")?
        .file_name(display_name)
        .mime_str("application/octet-stream")
        .map_err(|error| format!("Could not prepare {display_name} for upload: {error}"))?;
    let mut form = ureq::unversioned::multipart::Form::new().part("raw", raw);

    let sidecar_path = crate::sidecar::sidecar_path_for_raw(raw_path);
    if sidecar_path.is_file() {
        let sidecar = checked_upload_part(
            &sidecar_path,
            crate::sidecar::MAX_SIDECAR_BYTES,
            "sidecar",
        )?
        .file_name(&format!("{display_name}.auraw"))
        .mime_str("application/vnd.auraw.sidecar")
        .map_err(|error| format!("Could not prepare the sidecar for upload: {error}"))?;
        form = form.part("sidecar", sidecar);

        match crate::sidecar::developed_thumbnail_cache_is_fresh(raw_path) {
            Ok(true) => {
                let thumbnail_path = crate::sidecar::developed_thumbnail_path_for_raw(raw_path);
                let thumbnail =
                    checked_upload_part(&thumbnail_path, MAX_THUMBNAIL_BYTES, "thumbnail")?
                        .file_name(&format!("{display_name}.auraw-thumb.jpg"))
                        .mime_str("image/jpeg")
                        .map_err(|error| {
                            format!("Could not prepare the developed thumbnail: {error}")
                        })?;
                form = form.part("thumbnail", thumbnail);
            }
            Ok(false) => {}
            Err(error) => log::warn!(
                "could not validate the developed thumbnail for {display_name}; the server will generate a RAW preview instead: {error}"
            ),
        }
    }
    send_upload_form(config, form, folder_id)
}

pub fn upload_asset_file(
    config: &CloudConfig,
    raw: File,
    display_name: &str,
    declared_bytes: Option<u64>,
) -> Result<CloudAsset, String> {
    upload_asset_file_to_folder(
        config,
        raw,
        display_name,
        declared_bytes,
        CLOUD_ROOT_FOLDER_ID,
    )
}

pub fn upload_asset_file_to_folder<'a>(
    config: &CloudConfig,
    raw: File,
    display_name: &'a str,
    declared_bytes: Option<u64>,
    folder_id: &'a str,
) -> Result<CloudAsset, String> {
    upload_asset_file_with_sidecar_to_folder(
        config,
        raw,
        display_name,
        declared_bytes,
        None,
        folder_id,
    )
}

pub fn upload_asset_file_with_sidecar_to_folder<'a>(
    config: &CloudConfig,
    raw: File,
    display_name: &'a str,
    declared_bytes: Option<u64>,
    sidecar_path: Option<&Path>,
    folder_id: &'a str,
) -> Result<CloudAsset, String> {
    upload_asset_file_with_sidecar_and_thumbnail_to_folder(
        config,
        raw,
        display_name,
        declared_bytes,
        sidecar_path,
        None,
        folder_id,
    )
}

pub fn upload_asset_file_with_sidecar_and_thumbnail_to_folder<'a>(
    config: &CloudConfig,
    raw: File,
    display_name: &'a str,
    declared_bytes: Option<u64>,
    sidecar_path: Option<&Path>,
    thumbnail_path: Option<&Path>,
    folder_id: &'a str,
) -> Result<CloudAsset, String> {
    validate_upload_name(display_name)?;
    validate_upload_size(display_name, declared_bytes)?;
    let raw = ureq::unversioned::multipart::Part::owned_reader(raw.take(MAX_RAW_BYTES + 1))
        .file_name(display_name)
        .mime_str("application/octet-stream")
        .map_err(|error| format!("Could not prepare {display_name} for upload: {error}"))?;
    let mut form = ureq::unversioned::multipart::Form::new().part("raw", raw);
    if let Some(sidecar_path) = sidecar_path {
        let sidecar = checked_upload_part(
            sidecar_path,
            crate::sidecar::MAX_SIDECAR_BYTES,
            "sidecar",
        )?
        .file_name(&format!("{display_name}.auraw"))
        .mime_str("application/vnd.auraw.sidecar")
        .map_err(|error| format!("Could not prepare the sidecar for upload: {error}"))?;
        form = form.part("sidecar", sidecar);
    }
    if let Some(thumbnail_path) = thumbnail_path {
        let thumbnail = checked_upload_part(thumbnail_path, MAX_THUMBNAIL_BYTES, "thumbnail")?
            .file_name(&format!("{display_name}.auraw-thumb.jpg"))
            .mime_str("image/jpeg")
            .map_err(|error| format!("Could not prepare the developed thumbnail: {error}"))?;
        form = form.part("thumbnail", thumbnail);
    }
    send_upload_form(config, form, folder_id)
}

fn asset_cache_dir(cache_root: &Path, server_url: &str, asset_id: &str) -> PathBuf {
    let mut namespace_digest = Sha256Context::new(&SHA256);
    namespace_digest.update(server_url.as_bytes());
    cache_root
        .join("assets")
        .join(sha256_hex(namespace_digest))
        .join(asset_id)
}

fn metadata_path_for_directory(directory: &Path) -> PathBuf {
    directory.join("cloud.json")
}

fn metadata_path_for_raw(raw_path: &Path) -> Option<PathBuf> {
    let name = raw_path.file_name()?.to_str()?;
    if !name.starts_with("original.") {
        return None;
    }
    let directory = raw_path.parent()?;
    let asset_id = directory.file_name()?.to_str()?;
    validate_hex_identifier(asset_id, "cached asset ID").ok()?;
    let namespace = directory.parent()?.file_name()?.to_str()?;
    validate_hex_identifier(namespace, "cloud cache namespace").ok()?;
    Some(metadata_path_for_directory(directory))
}

pub fn cached_asset_id_for_raw(raw_path: &Path) -> Option<String> {
    let metadata_path = metadata_path_for_raw(raw_path)?;
    load_metadata(&metadata_path)
        .ok()
        .flatten()
        .map(|metadata| metadata.asset_id)
}

/// Reads the current local sync state for a cached cloud RAW without network
/// access. Save workers use this to update the matching library badge as soon
/// as their metadata changes.
pub fn cached_asset_sync_state(raw_path: &Path) -> Option<(String, CloudSyncState)> {
    let metadata_path = metadata_path_for_raw(raw_path)?;
    let metadata = load_metadata(&metadata_path).ok().flatten()?;
    let state = match metadata.sync_issue {
        Some(CachedSyncIssue::Conflict) => CloudSyncState::Conflict,
        Some(CachedSyncIssue::Failed) => CloudSyncState::Failed,
        None if metadata.pending_sidecar_upload => CloudSyncState::Queued,
        None => CloudSyncState::Synced,
    };
    Some((metadata.asset_id, state))
}

fn load_metadata(path: &Path) -> Result<Option<CachedAssetMetadata>, String> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() <= MAX_METADATA_BYTES => metadata,
        Ok(_) => return Err("Cloud cache metadata exceeds its safety limit.".to_owned()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not inspect cloud cache metadata: {error}")),
    };
    let _ = metadata;
    let bytes =
        fs::read(path).map_err(|error| format!("Could not read cloud metadata: {error}"))?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| format!("Cloud cache metadata is invalid: {error}"))
}

fn save_metadata(path: &Path, metadata: &CachedAssetMetadata) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(metadata)
        .map_err(|error| format!("Could not encode cloud cache metadata: {error}"))?;
    crate::thumbnail_cache::write_bytes_atomic(path, &bytes)
        .map_err(|error| format!("Could not save cloud cache metadata: {error}"))
}

fn sha256_hex(context: Sha256Context) -> String {
    context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn download_to_path(
    config: &CloudConfig,
    url: &str,
    destination: &Path,
    maximum_bytes: u64,
    expected_bytes: Option<u64>,
    expected_sha256: Option<&str>,
    mut progress: impl FnMut(u64, u64),
) -> Result<(), String> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create cloud cache: {error}"))?;
    }
    let temporary = destination.with_extension(format!(
        "{}.{}.part",
        destination
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("download"),
        std::process::id()
    ));
    let result = (|| {
        let mut response = get(config, url)
            .map_err(|error| format!("Could not download from AuRaw Cloud: {error}"))?;
        let declared = response.body().content_length();
        if declared.is_some_and(|bytes| bytes > maximum_bytes) {
            return Err("Cloud download exceeds the client safety limit.".to_owned());
        }
        let total = expected_bytes.or(declared).unwrap_or(0);
        let mut reader = response.body_mut().as_reader();
        let mut output = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temporary)
            .map_err(|error| format!("Could not create cloud cache file: {error}"))?;
        let mut downloaded = 0u64;
        let mut digest = Sha256Context::new(&SHA256);
        let mut buffer = [0u8; 256 * 1024];
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|error| format!("Cloud download stopped: {error}"))?;
            if count == 0 {
                break;
            }
            downloaded = downloaded
                .checked_add(count as u64)
                .ok_or_else(|| "Cloud download size overflowed.".to_owned())?;
            if downloaded > maximum_bytes {
                return Err("Cloud download exceeds the client safety limit.".to_owned());
            }
            digest.update(&buffer[..count]);
            output
                .write_all(&buffer[..count])
                .map_err(|error| format!("Could not write cloud cache file: {error}"))?;
            progress(downloaded, total);
        }
        if expected_bytes.is_some_and(|expected| expected != downloaded) {
            return Err(format!(
                "Cloud download was incomplete ({downloaded} of {} bytes).",
                expected_bytes.unwrap_or_default()
            ));
        }
        if expected_sha256.is_some_and(|expected| sha256_hex(digest) != expected) {
            return Err("Cloud download failed its integrity check.".to_owned());
        }
        output
            .sync_all()
            .map_err(|error| format!("Could not flush cloud cache file: {error}"))?;
        drop(output);
        crate::file_ops::replace_file(&temporary, destination)
            .map_err(|error| format!("Could not publish cloud cache file: {error}"))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

pub fn load_thumbnail(
    config: &CloudConfig,
    cache_root: &Path,
    asset: &CloudAsset,
    maximum_edge: u32,
    allow_network: bool,
) -> Result<RawThumbnail, String> {
    validate_asset(asset)?;
    let cache_path = cache_root
        .join("thumbnails")
        .join(format!("{}-{}.jpg", asset.id, asset.thumbnail_etag));
    match crate::thumbnail_cache::load_jpeg(&cache_path, maximum_edge) {
        Ok(Some(thumbnail)) => return Ok(thumbnail),
        Ok(None) => {}
        Err(error) => {
            log::warn!("Discarding invalid cloud thumbnail: {error}");
            let _ = fs::remove_file(&cache_path);
        }
    }
    if !allow_network {
        return Err("This cloud thumbnail has not been cached for offline use yet.".to_owned());
    }
    let url = config.endpoint(&format!(
        "/api/v1/assets/{}/thumbnail?v={}",
        asset.id, asset.thumbnail_etag
    ))?;
    download_to_path(
        config,
        &url,
        &cache_path,
        MAX_THUMBNAIL_BYTES,
        None,
        Some(&asset.thumbnail_etag),
        |_, _| {},
    )?;
    crate::thumbnail_cache::load_jpeg(&cache_path, maximum_edge)?
        .ok_or_else(|| "Cloud thumbnail was not a supported JPEG image.".to_owned())
}

fn config_from_metadata(metadata: &CachedAssetMetadata) -> CloudConfig {
    CloudConfig {
        enabled: true,
        server_url: metadata.server_url.clone(),
        access_token: metadata.access_token.clone(),
    }
}

enum SidecarUploadError {
    Retryable(String),
    Conflict(String),
    Fatal(String),
}

impl SidecarUploadError {
    fn message(self) -> String {
        match self {
            Self::Retryable(message) | Self::Conflict(message) | Self::Fatal(message) => message,
        }
    }

    fn sync_issue(&self) -> CachedSyncIssue {
        match self {
            Self::Conflict(_) => CachedSyncIssue::Conflict,
            Self::Retryable(_) | Self::Fatal(_) => CachedSyncIssue::Failed,
        }
    }
}

const CLOUD_EDIT_CONFLICT_PREFIX: &str = "Cloud edit conflict:";

pub fn is_sidecar_conflict_message(message: &str) -> bool {
    message.starts_with(CLOUD_EDIT_CONFLICT_PREFIX)
}

fn request_sidecar_upload(
    metadata: &mut CachedAssetMetadata,
    sidecar_path: &Path,
) -> Result<(), SidecarUploadError> {
    let bytes = crate::sidecar::read_bounded(sidecar_path)
        .map_err(|error| SidecarUploadError::Fatal(error.to_string()))?;
    let config = config_from_metadata(metadata);
    let url = config
        .endpoint(&format!("/api/v1/assets/{}/sidecar", metadata.asset_id))
        .map_err(SidecarUploadError::Fatal)?;
    let request = agent()
        .put(&url)
        .header("Content-Type", "application/vnd.auraw.sidecar");
    let request = if let Some(value) = authorization(&config) {
        request.header("Authorization", value)
    } else {
        request
    };
    let request = if let Some(etag) = &metadata.sidecar_etag {
        request.header("If-Match", format!("\"{etag}\""))
    } else {
        request.header("If-None-Match", "*")
    };
    let response = request.send(&bytes).map_err(|error| {
        let message = match error {
            ureq::Error::StatusCode(412) => {
                return SidecarUploadError::Conflict(
                    "Cloud edit conflict: another client saved this image first. Your local cached sidecar was preserved and remains marked as waiting; the server copy was not overwritten."
                        .to_owned(),
                );
            }
            ureq::Error::StatusCode(401) => {
                return SidecarUploadError::Fatal(
                    "AuRaw Cloud rejected the access token.".to_owned(),
                );
            }
            ureq::Error::StatusCode(status) if status < 500 && status != 408 && status != 429 => {
                return SidecarUploadError::Fatal(format!(
                    "AuRaw Cloud rejected the sidecar with HTTP status {status}."
                ));
            }
            _ => format!(
                "The sidecar was saved locally but could not sync to AuRaw Cloud: {error}"
            ),
        };
        SidecarUploadError::Retryable(message)
    })?;
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_matches('"').to_owned())
        .ok_or_else(|| {
            SidecarUploadError::Fatal(
                "AuRaw Cloud saved the sidecar without returning its version.".to_owned(),
            )
        })?;
    validate_hex_identifier(&etag, "sidecar version").map_err(SidecarUploadError::Fatal)?;
    metadata.sidecar_etag = Some(etag);
    metadata.pending_sidecar_upload = false;
    metadata.sync_issue = None;
    Ok(())
}

pub fn sync_sidecar_if_cloud_cached(
    raw_path: &Path,
    allow_network: bool,
) -> Result<Option<String>, String> {
    let Some(metadata_path) = metadata_path_for_raw(raw_path) else {
        return Ok(None);
    };
    let Some(mut metadata) = load_metadata(&metadata_path)? else {
        return Ok(None);
    };
    let sidecar_path = crate::sidecar::sidecar_path_for_raw(raw_path);
    metadata.pending_sidecar_upload = true;
    metadata.sync_issue = None;
    save_metadata(&metadata_path, &metadata)?;
    if !allow_network {
        return Ok(Some(format!(
            "AuRaw Cloud ({}) · waiting to sync",
            metadata.server_url
        )));
    }
    match request_sidecar_upload(&mut metadata, &sidecar_path) {
        Ok(()) => {
            save_metadata(&metadata_path, &metadata)?;
            Ok(Some(format!("AuRaw Cloud ({})", metadata.server_url)))
        }
        Err(error) => {
            let retryable = matches!(error, SidecarUploadError::Retryable(_));
            metadata.sync_issue = Some(error.sync_issue());
            save_metadata(&metadata_path, &metadata)?;
            let message = error.message();
            if retryable {
                log::warn!("{message}");
                Ok(Some(format!(
                    "AuRaw Cloud ({}) · sync failed; queued for retry",
                    metadata.server_url
                )))
            } else {
                Err(message)
            }
        }
    }
}

fn current_asset_for_metadata(metadata: &CachedAssetMetadata) -> Result<CloudAsset, String> {
    let config = config_from_metadata(metadata);
    let catalog = fetch_catalog(&config).map_err(CatalogFetchError::message)?;
    let asset = catalog
        .items
        .into_iter()
        .find(|asset| asset.id == metadata.asset_id)
        .ok_or_else(|| "This RAW is no longer present in AuRaw Cloud.".to_owned())?;
    if asset.raw_etag != metadata.raw_etag {
        return Err(
            "The server RAW no longer matches the cached file. Reopen it from Cloud before resolving edits."
                .to_owned(),
        );
    }
    Ok(asset)
}

/// Keeps the cached sidecar and deliberately replaces the server's latest
/// sidecar. The latest ETag is fetched immediately before the conditional PUT,
/// so a second concurrent save still produces a conflict rather than silently
/// overwriting a newer revision.
pub fn overwrite_server_sidecar_with_local(raw_path: &Path) -> Result<String, String> {
    let metadata_path = metadata_path_for_raw(raw_path)
        .ok_or_else(|| "This file is not an AuRaw Cloud cache entry.".to_owned())?;
    let mut metadata = load_metadata(&metadata_path)?
        .ok_or_else(|| "The cloud cache metadata is missing.".to_owned())?;
    let latest = current_asset_for_metadata(&metadata)?;
    metadata.sidecar_etag = latest.sidecar_etag;
    metadata.thumbnail_etag = latest.thumbnail_etag;
    metadata.pending_sidecar_upload = true;
    metadata.sync_issue = None;
    let sidecar_path = crate::sidecar::sidecar_path_for_raw(raw_path);
    if let Err(error) = request_sidecar_upload(&mut metadata, &sidecar_path) {
        metadata.sync_issue = Some(error.sync_issue());
        save_metadata(&metadata_path, &metadata)?;
        return Err(error.message());
    }
    save_metadata(&metadata_path, &metadata)?;
    Ok(format!("AuRaw Cloud ({})", metadata.server_url))
}

/// Discards the cached sidecar and installs the server's latest sidecar (or no
/// sidecar when the server image is unedited). A confirmation catalog read
/// prevents a sidecar revision that changes during download from being
/// published as the resolved local copy.
pub fn overwrite_local_sidecar_with_server(raw_path: &Path) -> Result<String, String> {
    let metadata_path = metadata_path_for_raw(raw_path)
        .ok_or_else(|| "This file is not an AuRaw Cloud cache entry.".to_owned())?;
    let mut metadata = load_metadata(&metadata_path)?
        .ok_or_else(|| "The cloud cache metadata is missing.".to_owned())?;
    let config = config_from_metadata(&metadata);
    let sidecar_path = crate::sidecar::sidecar_path_for_raw(raw_path);
    let remote_sidecar_path =
        sidecar_path.with_extension(format!("auraw.server-conflict-{}", std::process::id()));

    let result = (|| {
        let mut latest = current_asset_for_metadata(&metadata)?;
        for attempt in 0..2 {
            if let Some(etag) = latest.sidecar_etag.as_deref() {
                let url =
                    config.endpoint(&format!("/api/v1/assets/{}/sidecar", metadata.asset_id))?;
                download_to_path(
                    &config,
                    &url,
                    &remote_sidecar_path,
                    crate::sidecar::MAX_SIDECAR_BYTES,
                    None,
                    Some(etag),
                    |_, _| {},
                )?;
            }

            let confirmed = current_asset_for_metadata(&metadata)?;
            if confirmed.sidecar_etag != latest.sidecar_etag {
                if attempt == 0 {
                    latest = confirmed;
                    continue;
                }
                return Err(
                    "The server edits changed again while resolving the conflict. Try again."
                        .to_owned(),
                );
            }

            if latest.sidecar_etag.is_some() {
                crate::file_ops::replace_file(&remote_sidecar_path, &sidecar_path)
                    .map_err(|error| format!("Could not install the server sidecar: {error}"))?;
            } else {
                if let Err(error) = fs::remove_file(&sidecar_path) {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        return Err(format!("Could not remove the local sidecar: {error}"));
                    }
                }
                let _ = fs::remove_file(&remote_sidecar_path);
            }

            #[cfg(not(target_os = "android"))]
            crate::sidecar::invalidate_developed_thumbnail_cache(raw_path)?;
            metadata.sidecar_etag = latest.sidecar_etag;
            metadata.thumbnail_etag = latest.thumbnail_etag;
            metadata.pending_sidecar_upload = false;
            metadata.sync_issue = None;
            save_metadata(&metadata_path, &metadata)?;
            return Ok(format!("AuRaw Cloud ({})", metadata.server_url));
        }
        unreachable!("the bounded conflict loop always returns")
    })();
    if result.is_err() {
        let _ = fs::remove_file(&remote_sidecar_path);
    }
    result
}

fn refresh_sidecar(
    config: &CloudConfig,
    asset: &CloudAsset,
    raw_path: &Path,
    metadata_path: &Path,
    metadata: &mut CachedAssetMetadata,
) -> Result<(), String> {
    let sidecar_path = crate::sidecar::sidecar_path_for_raw(raw_path);
    if metadata.pending_sidecar_upload && sidecar_path.is_file() {
        // The local sidecar is the user's recovery copy. A transient outage or
        // an optimistic-concurrency conflict must not make the already cached
        // RAW impossible to reopen; keep the pending marker and retry on the
        // next save/open instead.
        if let Err(error) = request_sidecar_upload(metadata, &sidecar_path) {
            metadata.sync_issue = Some(error.sync_issue());
            log::warn!(
                "cloud sidecar remains pending while reopening the cache: {}",
                error.message()
            );
        }
        save_metadata(metadata_path, metadata)?;
        return Ok(());
    }
    if metadata.pending_sidecar_upload {
        // The pending recovery file was removed outside AuRaw, so there is no
        // local edit left to upload. Resume normal remote-sidecar refresh.
        metadata.pending_sidecar_upload = false;
        metadata.sync_issue = None;
    }
    match &asset.sidecar_etag {
        Some(remote_etag)
            if metadata.sidecar_etag.as_ref() == Some(remote_etag) && sidecar_path.is_file() =>
        {
            Ok(())
        }
        Some(remote_etag) => {
            let url = config.endpoint(&format!("/api/v1/assets/{}/sidecar", asset.id))?;
            download_to_path(
                config,
                &url,
                &sidecar_path,
                crate::sidecar::MAX_SIDECAR_BYTES,
                None,
                Some(remote_etag),
                |_, _| {},
            )?;
            metadata.sidecar_etag = Some(remote_etag.clone());
            Ok(())
        }
        None => {
            if sidecar_path.is_file() {
                fs::remove_file(&sidecar_path).map_err(|error| {
                    format!("Could not clear a stale cached cloud sidecar: {error}")
                })?;
            }
            metadata.sidecar_etag = None;
            Ok(())
        }
    }
}

pub fn download_asset(
    config: &CloudConfig,
    cache_root: &Path,
    asset: &CloudAsset,
    progress: impl FnMut(u64, u64),
) -> Result<CachedCloudAsset, String> {
    validate_asset(asset)?;
    let config = config.normalized()?;
    // Keep sidecars from different servers in different directories even when
    // both servers contain the same content-addressed RAW. This prevents a
    // server switch from replacing an unsynced local edit for that RAW.
    let directory = asset_cache_dir(cache_root, &config.server_url, &asset.id);
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create cloud asset cache: {error}"))?;
    let raw_extension = Path::new(&asset.name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("raw");
    let raw_path = directory.join(format!("original.{raw_extension}"));
    let metadata_path = metadata_path_for_directory(&directory);
    let existing = load_metadata(&metadata_path)?;
    let metadata_matches_asset = existing.as_ref().is_some_and(|metadata| {
        metadata.schema_version == 1
            && metadata.asset_id == asset.id
            && metadata.server_url == config.server_url
            && metadata.raw_etag == asset.raw_etag
    });
    let raw_is_current = metadata_matches_asset
        && raw_path.is_file()
        && fs::metadata(&raw_path).is_ok_and(|file| file.len() == asset.bytes);
    if !raw_is_current {
        let url = config.endpoint(&format!("/api/v1/assets/{}/raw", asset.id))?;
        download_to_path(
            &config,
            &url,
            &raw_path,
            MAX_RAW_BYTES,
            Some(asset.bytes),
            Some(&asset.raw_etag),
            progress,
        )?;
    }
    let mut metadata = existing
        .filter(|_| metadata_matches_asset)
        .unwrap_or(CachedAssetMetadata {
            schema_version: 1,
            asset_id: asset.id.clone(),
            server_url: config.server_url.clone(),
            access_token: config.access_token.clone(),
            raw_etag: asset.raw_etag.clone(),
            sidecar_etag: None,
            thumbnail_etag: asset.thumbnail_etag.clone(),
            pending_sidecar_upload: false,
            sync_issue: None,
        });
    metadata.schema_version = 1;
    metadata.asset_id = asset.id.clone();
    metadata.server_url = config.server_url.clone();
    metadata.access_token = config.access_token.clone();
    metadata.raw_etag = asset.raw_etag.clone();
    metadata.thumbnail_etag = asset.thumbnail_etag.clone();
    refresh_sidecar(&config, asset, &raw_path, &metadata_path, &mut metadata)?;
    save_metadata(&metadata_path, &metadata)?;
    Ok(CachedCloudAsset {
        asset_id: asset.id.clone(),
        raw_path,
        label: asset.name.clone(),
        offline_reason: None,
    })
}

fn cached_raw_path(directory: &Path, asset: &CloudAsset) -> Result<PathBuf, String> {
    let raw_extension = Path::new(&asset.name)
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or("raw");
    let preferred = directory.join(format!("original.{raw_extension}"));
    if preferred.is_file() {
        return Ok(preferred);
    }
    let mut candidates = fs::read_dir(directory)
        .map_err(|error| format!("Could not inspect the cached cloud RAW: {error}"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("original."))
        });
    let Some(candidate) = candidates.next() else {
        return Err(
            "This RAW has not been downloaded on this device and is unavailable offline."
                .to_owned(),
        );
    };
    if candidates.next().is_some() {
        return Err("The cached cloud RAW is ambiguous and must be downloaded again.".to_owned());
    }
    Ok(candidate)
}

fn open_cached_asset(
    config: &CloudConfig,
    cache_root: &Path,
    asset: &CloudAsset,
    offline_reason: String,
) -> Result<CachedCloudAsset, String> {
    validate_asset(asset)?;
    let config = config.normalized()?;
    let directory = asset_cache_dir(cache_root, &config.server_url, &asset.id);
    let metadata_path = metadata_path_for_directory(&directory);
    let metadata = load_metadata(&metadata_path)?.ok_or_else(|| {
        "This RAW has not been downloaded on this device and is unavailable offline.".to_owned()
    })?;
    if metadata.schema_version != 1
        || metadata.asset_id != asset.id
        || metadata.server_url != config.server_url
        || metadata.access_token != config.access_token
        || metadata.raw_etag != asset.raw_etag
    {
        return Err(
            "The cached RAW is from an older cloud version and must be downloaded again."
                .to_owned(),
        );
    }
    let raw_path = cached_raw_path(&directory, asset)?;
    if !fs::metadata(&raw_path).is_ok_and(|file| file.is_file() && file.len() == asset.bytes) {
        return Err("The cached cloud RAW is incomplete and must be downloaded again.".to_owned());
    }
    let sidecar_path = crate::sidecar::sidecar_path_for_raw(&raw_path);
    if (metadata.pending_sidecar_upload || metadata.sidecar_etag.is_some())
        && !sidecar_path.is_file()
    {
        return Err("The cached cloud edit is incomplete and must be downloaded again.".to_owned());
    }
    Ok(CachedCloudAsset {
        asset_id: asset.id.clone(),
        raw_path,
        label: asset.name.clone(),
        offline_reason: Some(offline_reason),
    })
}

pub fn asset_available_offline(
    config: &CloudConfig,
    cache_root: &Path,
    asset: &CloudAsset,
) -> bool {
    open_cached_asset(config, cache_root, asset, "offline cache check".to_owned()).is_ok()
}

pub fn asset_sync_state(
    config: &CloudConfig,
    cache_root: &Path,
    asset: &CloudAsset,
) -> CloudSyncState {
    let Ok(config) = config.normalized() else {
        return CloudSyncState::Failed;
    };
    let metadata_path = metadata_path_for_directory(&asset_cache_dir(
        cache_root,
        &config.server_url,
        &asset.id,
    ));
    let Ok(Some(metadata)) = load_metadata(&metadata_path) else {
        return CloudSyncState::Synced;
    };
    match metadata.sync_issue {
        Some(CachedSyncIssue::Conflict) => CloudSyncState::Conflict,
        Some(CachedSyncIssue::Failed) => CloudSyncState::Failed,
        None if metadata.pending_sidecar_upload => CloudSyncState::Queued,
        None => CloudSyncState::Synced,
    }
}

/// Returns the validated cached RAW path without performing network I/O.
///
/// Filmstrip entries use this to associate a cloud catalog asset with the
/// document currently open in Develop while retaining the server asset as the
/// authoritative identity for future opens.
pub fn cached_asset_path(
    config: &CloudConfig,
    cache_root: &Path,
    asset: &CloudAsset,
) -> Option<PathBuf> {
    open_cached_asset(config, cache_root, asset, "offline cache check".to_owned())
        .ok()
        .map(|cached| cached.raw_path)
}

fn version_race(error: &str) -> bool {
    error.contains("integrity check") || error.contains("download was incomplete")
}

/// Opens a catalog entry by identity rather than trusting the version embedded
/// in an already-rendered card. This closes the race where another client saves
/// a sidecar while the library page remains open.
pub fn open_asset(
    config: &CloudConfig,
    cache_root: &Path,
    selected: &CloudAsset,
    allow_network: bool,
    mut progress: impl FnMut(u64, u64),
) -> Result<CachedCloudAsset, String> {
    validate_asset(selected)?;
    let snapshot = list_assets_cached(config, cache_root, allow_network)?;
    let current = snapshot
        .items
        .iter()
        .find(|asset| asset.id == selected.id)
        .cloned()
        .ok_or_else(|| "This RAW is no longer present in AuRaw Cloud.".to_owned())?;
    if let Some(reason) = snapshot.offline_reason {
        return open_cached_asset(config, cache_root, &current, reason);
    }

    match download_asset(config, cache_root, &current, |downloaded, total| {
        progress(downloaded, total)
    }) {
        Ok(cached) => Ok(cached),
        Err(first_error) if version_race(&first_error) => {
            // A save can land after the catalog GET but before the sidecar GET.
            // Fetch the version again and make one bounded retry.
            let retry = fetch_catalog(config).map_err(CatalogFetchError::message)?;
            if let Err(error) = save_catalog_cache(config, cache_root, &retry) {
                log::warn!("{error}");
            }
            let current = retry
                .items
                .iter()
                .find(|asset| asset.id == selected.id)
                .ok_or_else(|| "This RAW is no longer present in AuRaw Cloud.".to_owned())?;
            download_asset(config, cache_root, current, |downloaded, total| {
                progress(downloaded, total)
            })
            .map_err(|retry_error| {
                format!(
                    "AuRaw Cloud changed while this RAW was opening. Refresh and try again: {retry_error}"
                )
            })
        }
        Err(error) => Err(error),
    }
}

/// Resolves a selection against one catalog snapshot and prepares every RAW in
/// selection order. This keeps multi-file exports and adjustment operations
/// from issuing a separate catalog request for every selected card.
pub fn open_assets(
    config: &CloudConfig,
    cache_root: &Path,
    selected: &[CloudAsset],
    allow_network: bool,
) -> Result<Vec<CachedCloudAsset>, String> {
    for asset in selected {
        validate_asset(asset)?;
    }
    if selected.is_empty() {
        return Ok(Vec::new());
    }

    let snapshot = list_assets_cached(config, cache_root, allow_network)?;
    let current = selected
        .iter()
        .map(|selected| {
            snapshot
                .items
                .iter()
                .find(|asset| asset.id == selected.id)
                .cloned()
                .ok_or_else(|| format!("{} is no longer present in AuRaw Cloud.", selected.name))
        })
        .collect::<Result<Vec<_>, _>>()?;

    if let Some(reason) = snapshot.offline_reason {
        return current
            .iter()
            .map(|asset| {
                open_cached_asset(config, cache_root, asset, reason.clone())
                    .map_err(|error| format!("Could not prepare {}: {error}", asset.name))
            })
            .collect();
    }

    let mut cached = Vec::with_capacity(current.len());
    for asset in current {
        let prepared = match download_asset(config, cache_root, &asset, |_, _| {}) {
            Ok(cached) => Ok(cached),
            Err(first_error) if version_race(&first_error) => {
                // Keep the same bounded retry used by single-card opening. A
                // sidecar save can otherwise land between the shared catalog
                // read and this particular asset download.
                let retry = fetch_catalog(config).map_err(CatalogFetchError::message)?;
                if let Err(error) = save_catalog_cache(config, cache_root, &retry) {
                    log::warn!("{error}");
                }
                let current = retry
                    .items
                    .iter()
                    .find(|candidate| candidate.id == asset.id)
                    .ok_or_else(|| {
                        format!("{} is no longer present in AuRaw Cloud.", asset.name)
                    })?;
                download_asset(config, cache_root, current, |_, _| {}).map_err(|retry_error| {
                    format!(
                        "AuRaw Cloud changed while {} was being prepared. Refresh and try again: {retry_error}",
                        asset.name
                    )
                })
            }
            Err(error) => Err(error),
        }
        .map_err(|error| format!("Could not prepare {}: {error}", asset.name))?;
        cached.push(prepared);
    }
    Ok(cached)
}

pub fn upload_developed_thumbnail_if_cloud_cached(
    raw_path: &Path,
    thumbnail: &RawThumbnail,
) -> Result<bool, String> {
    let Some(metadata_path) = metadata_path_for_raw(raw_path) else {
        return Ok(false);
    };
    let Some(mut metadata) = load_metadata(&metadata_path)? else {
        return Ok(false);
    };
    let rgba =
        image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.rgba.clone())
            .ok_or_else(|| "Cloud thumbnail pixels do not match its dimensions.".to_owned())?;
    let rgb = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(
        &mut jpeg,
        crate::thumbnail_cache::THUMBNAIL_JPEG_QUALITY,
    )
    .encode(
        rgb.as_raw(),
        thumbnail.width,
        thumbnail.height,
        image::ExtendedColorType::Rgb8,
    )
    .map_err(|error| format!("Could not encode cloud thumbnail: {error}"))?;
    let config = config_from_metadata(&metadata);
    let url = config.endpoint(&format!("/api/v1/assets/{}/thumbnail", metadata.asset_id))?;
    let request = agent().put(&url).header("Content-Type", "image/jpeg");
    let request = if let Some(value) = authorization(&config) {
        request.header("Authorization", value)
    } else {
        request
    };
    let request = match &metadata.sidecar_etag {
        Some(etag) => request.header("X-AuRaw-Sidecar-ETag", format!("\"{etag}\"")),
        None => request,
    };
    let response = match request.send(&jpeg) {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(412)) => {
            metadata.sync_issue = Some(CachedSyncIssue::Conflict);
            save_metadata(&metadata_path, &metadata)?;
            return Err(
                "The developed cloud thumbnail belongs to an older sidecar revision.".to_owned(),
            );
        }
        Err(ureq::Error::StatusCode(401)) => {
            metadata.sync_issue = Some(CachedSyncIssue::Failed);
            save_metadata(&metadata_path, &metadata)?;
            return Err("AuRaw Cloud rejected the access token.".to_owned());
        }
        Err(ureq::Error::StatusCode(status)) if status < 500 => {
            metadata.sync_issue = Some(CachedSyncIssue::Failed);
            save_metadata(&metadata_path, &metadata)?;
            return Err(format!(
                "AuRaw Cloud rejected the developed thumbnail with HTTP status {status}."
            ));
        }
        Err(error) => {
            // The sidecar remains the authoritative edit. A thumbnail can be
            // rendered and uploaded again after connectivity returns, so a
            // transient preview failure must not turn an offline edit into a
            // save failure on Android.
            log::warn!("Could not sync the developed cloud thumbnail: {error}");
            metadata.sync_issue = Some(CachedSyncIssue::Failed);
            save_metadata(&metadata_path, &metadata)?;
            return Ok(true);
        }
    };
    if let Some(etag) = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_matches('"').to_owned())
    {
        validate_hex_identifier(&etag, "thumbnail version")?;
        metadata.thumbnail_etag = etag;
    }
    metadata.sync_issue = None;
    save_metadata(&metadata_path, &metadata)?;
    Ok(true)
}

pub fn cache_root(settings_path: Option<&Path>) -> Option<PathBuf> {
    settings_path
        .and_then(Path::parent)
        .map(|parent| parent.join("cloud-cache"))
}

pub fn cached_status(raw_path: Option<&Path>) -> Option<&'static str> {
    let raw_path = raw_path?;
    let metadata_path = metadata_path_for_raw(raw_path)?;
    let metadata = load_metadata(&metadata_path).ok().flatten()?;
    Some(match metadata.sync_issue {
        Some(CachedSyncIssue::Conflict) => "Cloud · edit conflict",
        Some(CachedSyncIssue::Failed) => "Cloud · sync failed",
        None if metadata.pending_sidecar_upload => "Cloud · waiting to sync",
        None => "Cloud · synced",
    })
}

pub fn modified_time(seconds: u64) -> SystemTime {
    UNIX_EPOCH
        .checked_add(Duration::from_secs(seconds))
        .unwrap_or(UNIX_EPOCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(not(target_os = "android"))]
    use std::io::{Read, Write};
    #[cfg(not(target_os = "android"))]
    use std::net::TcpListener;

    fn test_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!(
            "auraw-cloud-{label}-{}-{nonce}",
            std::process::id()
        ))
    }

    #[cfg(not(target_os = "android"))]
    fn read_test_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "client closed before sending HTTP headers");
            request.extend_from_slice(&buffer[..count]);
            if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let content_length = String::from_utf8_lossy(&request[..header_end])
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().unwrap())
            })
            .unwrap_or(0);
        while request.len() < header_end + content_length {
            let count = stream.read(&mut buffer).unwrap();
            assert!(count > 0, "client closed before sending the HTTP body");
            request.extend_from_slice(&buffer[..count]);
        }
        request
    }

    #[cfg(not(target_os = "android"))]
    fn write_test_http_response(
        stream: &mut std::net::TcpStream,
        status: &str,
        content_type: &str,
        extra_headers: &str,
        body: &[u8],
    ) {
        write!(
            stream,
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n{extra_headers}Connection: close\r\n\r\n",
            body.len()
        )
        .unwrap();
        stream.write_all(body).unwrap();
    }

    fn digest_bytes(bytes: &[u8]) -> String {
        let mut digest = Sha256Context::new(&SHA256);
        digest.update(bytes);
        sha256_hex(digest)
    }

    fn test_asset(raw: &[u8], sidecar_etag: Option<String>) -> CloudAsset {
        let raw_etag = digest_bytes(raw);
        CloudAsset {
            id: raw_etag.clone(),
            name: "offline-test.dng".to_owned(),
            bytes: raw.len() as u64,
            modified_seconds: 1,
            width: 512,
            height: 341,
            raw_etag,
            sidecar_etag,
            thumbnail_etag: "d".repeat(64),
            thumbnail_kind: CloudThumbnailKind::Edited,
            folder_id: CLOUD_ROOT_FOLDER_ID.to_owned(),
        }
    }

    fn test_catalog(asset: &CloudAsset) -> CloudCatalog {
        CloudCatalog {
            items: vec![asset.clone()],
            folders: Vec::new(),
        }
    }

    #[test]
    fn normalizes_bare_lan_address() {
        let config = CloudConfig {
            enabled: true,
            server_url: " 192.168.1.20:8787/ ".to_owned(),
            access_token: " token ".to_owned(),
        }
        .normalized()
        .unwrap();
        assert_eq!(config.server_url, "http://192.168.1.20:8787");
        assert_eq!(config.access_token, "token");
    }

    #[test]
    fn rejects_non_http_cloud_address() {
        let error = CloudConfig {
            enabled: true,
            server_url: "ftp://photos.local".to_owned(),
            access_token: String::new(),
        }
        .normalized()
        .unwrap_err();
        assert!(error.contains("http:// or https://"));
    }

    #[test]
    fn asset_cache_is_namespaced_by_server() {
        let root = Path::new("cache");
        let first = asset_cache_dir(root, "https://one.example", &"a".repeat(64));
        let second = asset_cache_dir(root, "https://two.example", &"a".repeat(64));
        assert_ne!(first, second);
    }

    #[test]
    fn upload_validation_accepts_only_safe_raw_filenames() {
        assert!(validate_upload_name("holiday.DNG").is_ok());
        assert!(validate_upload_name("../holiday.dng").is_err());
        assert!(validate_upload_name("bad\"name.dng").is_err());
        assert!(validate_upload_name("bad\nname.dng").is_err());
        assert!(validate_upload_name("holiday.jpg").is_err());
        assert!(validate_upload_name("").is_err());
        assert!(validate_upload_size("empty.dng", Some(0)).is_err());
        assert!(validate_upload_size("photo.dng", Some(1024)).is_ok());
    }

    #[test]
    fn legacy_flat_catalog_defaults_assets_to_the_cloud_root() {
        let raw = b"legacy-raw";
        let raw_etag = digest_bytes(raw);
        let json = format!(
            r#"{{"items":[{{"id":"{raw_etag}","name":"legacy.dng","bytes":{},"modified_seconds":1,"width":512,"height":341,"raw_etag":"{raw_etag}","sidecar_etag":null,"thumbnail_etag":"{}"}}]}}"#,
            raw.len(),
            "d".repeat(64),
        );
        let catalog: CloudCatalog = serde_json::from_str(&json).unwrap();
        validate_catalog(&catalog).unwrap();
        assert!(catalog.folders.is_empty());
        assert_eq!(catalog.items[0].folder_id, CLOUD_ROOT_FOLDER_ID);
        assert_eq!(catalog.items[0].thumbnail_kind, CloudThumbnailKind::Legacy);
    }

    #[test]
    fn catalog_parses_explicit_thumbnail_provenance() {
        let raw = b"raw-preview";
        let raw_etag = digest_bytes(raw);
        let json = format!(
            r#"{{"items":[{{"id":"{raw_etag}","name":"preview.dng","bytes":{},"modified_seconds":1,"width":512,"height":341,"raw_etag":"{raw_etag}","sidecar_etag":null,"thumbnail_etag":"{}","thumbnail_kind":"raw","folder_id":"root"}}],"folders":[]}}"#,
            raw.len(),
            "d".repeat(64),
        );
        let catalog: CloudCatalog = serde_json::from_str(&json).unwrap();
        validate_catalog(&catalog).unwrap();
        assert_eq!(catalog.items[0].thumbnail_kind, CloudThumbnailKind::Raw);
    }

    #[test]
    fn catalog_validation_accepts_nested_folders_and_rejects_cycles() {
        let parent = CloudFolder {
            id: "a".repeat(64),
            parent_id: CLOUD_ROOT_FOLDER_ID.to_owned(),
            name: "Trips".to_owned(),
        };
        let child = CloudFolder {
            id: "b".repeat(64),
            parent_id: parent.id.clone(),
            name: "Day 1".to_owned(),
        };
        let mut asset = test_asset(b"nested", None);
        asset.folder_id = child.id.clone();
        let catalog = CloudCatalog {
            items: vec![asset],
            folders: vec![parent.clone(), child.clone()],
        };
        validate_catalog(&catalog).unwrap();

        let cyclic = CloudCatalog {
            items: Vec::new(),
            folders: vec![
                CloudFolder {
                    parent_id: child.id.clone(),
                    ..parent
                },
                child,
            ],
        };
        assert!(validate_catalog(&cyclic).unwrap_err().contains("cyclic"));
    }

    #[test]
    fn cached_catalog_is_available_offline_and_scoped_to_the_token() {
        let directory = test_directory("catalog-offline");
        let config = CloudConfig {
            enabled: true,
            server_url: "http://cloud.test:8787".to_owned(),
            access_token: "account-one".to_owned(),
        };
        let asset = test_asset(b"raw-body", None);
        save_catalog_cache(&config, &directory, &test_catalog(&asset)).unwrap();

        let snapshot = list_assets_cached(&config, &directory, false).unwrap();
        assert_eq!(snapshot.items, vec![asset]);
        assert!(snapshot.offline_reason.is_some());

        let other_account = CloudConfig {
            access_token: "account-two".to_owned(),
            ..config
        };
        assert!(list_assets_cached(&other_account, &directory, false)
            .unwrap_err()
            .contains("Connect once"));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn offline_open_uses_only_a_complete_previously_verified_cache() {
        let directory = test_directory("raw-offline");
        let config = CloudConfig {
            enabled: true,
            server_url: "http://cloud.test:8787".to_owned(),
            access_token: "test-token".to_owned(),
        };
        let raw = b"cached-raw-body";
        let asset = test_asset(raw, None);
        save_catalog_cache(&config, &directory, &test_catalog(&asset)).unwrap();
        let normalized = config.normalized().unwrap();
        let asset_directory = asset_cache_dir(&directory, &normalized.server_url, &asset.id);
        fs::create_dir_all(&asset_directory).unwrap();
        let raw_path = asset_directory.join("original.dng");
        fs::write(&raw_path, raw).unwrap();
        save_metadata(
            &metadata_path_for_directory(&asset_directory),
            &CachedAssetMetadata {
                schema_version: 1,
                asset_id: asset.id.clone(),
                server_url: normalized.server_url,
                access_token: normalized.access_token,
                raw_etag: asset.raw_etag.clone(),
                sidecar_etag: None,
                thumbnail_etag: asset.thumbnail_etag.clone(),
                pending_sidecar_upload: false,
                sync_issue: None,
            },
        )
        .unwrap();

        assert!(asset_available_offline(&config, &directory, &asset));
        let cached = open_asset(&config, &directory, &asset, false, |_, _| {}).unwrap();
        assert_eq!(cached.asset_id, asset.id);
        assert_eq!(cached.raw_path, raw_path);
        assert!(cached.offline_reason.is_some());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn offline_sidecar_save_is_marked_for_later_sync() {
        let root = test_directory("sidecar-offline");
        let directory = root.join("f".repeat(64)).join("a".repeat(64));
        let raw_path = directory.join("original.dng");
        fs::create_dir_all(&directory).unwrap();
        fs::write(&raw_path, b"raw").unwrap();
        fs::write(
            crate::sidecar::sidecar_path_for_raw(&raw_path),
            b"locally-saved-sidecar",
        )
        .unwrap();
        save_metadata(
            &metadata_path_for_directory(&directory),
            &CachedAssetMetadata {
                schema_version: 1,
                asset_id: "a".repeat(64),
                server_url: "http://cloud.test:8787".to_owned(),
                access_token: "test-token".to_owned(),
                raw_etag: "b".repeat(64),
                sidecar_etag: None,
                thumbnail_etag: "c".repeat(64),
                pending_sidecar_upload: false,
                sync_issue: None,
            },
        )
        .unwrap();

        let location = sync_sidecar_if_cloud_cached(&raw_path, false)
            .unwrap()
            .unwrap();
        assert!(location.contains("waiting to sync"));
        assert_eq!(
            cached_status(Some(&raw_path)),
            Some("Cloud · waiting to sync")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cached_sync_state_distinguishes_queue_failure_and_conflict() {
        let root = test_directory("sync-state");
        let config = CloudConfig {
            enabled: true,
            server_url: "http://cloud.test:8787".to_owned(),
            access_token: "test-token".to_owned(),
        };
        let asset = test_asset(b"sync-state-raw", None);
        let normalized = config.normalized().unwrap();
        let directory = asset_cache_dir(&root, &normalized.server_url, &asset.id);
        fs::create_dir_all(&directory).unwrap();
        let metadata_path = metadata_path_for_directory(&directory);
        let mut metadata = CachedAssetMetadata {
            schema_version: 1,
            asset_id: asset.id.clone(),
            server_url: normalized.server_url,
            access_token: normalized.access_token,
            raw_etag: asset.raw_etag.clone(),
            sidecar_etag: None,
            thumbnail_etag: asset.thumbnail_etag.clone(),
            pending_sidecar_upload: true,
            sync_issue: None,
        };
        let raw_path = directory.join("original.dng");

        save_metadata(&metadata_path, &metadata).unwrap();
        assert_eq!(asset_sync_state(&config, &root, &asset), CloudSyncState::Queued);
        assert_eq!(
            cached_asset_sync_state(&raw_path),
            Some((asset.id.clone(), CloudSyncState::Queued))
        );

        metadata.sync_issue = Some(CachedSyncIssue::Failed);
        save_metadata(&metadata_path, &metadata).unwrap();
        assert_eq!(asset_sync_state(&config, &root, &asset), CloudSyncState::Failed);
        assert_eq!(
            cached_asset_sync_state(&raw_path),
            Some((asset.id.clone(), CloudSyncState::Failed))
        );

        metadata.sync_issue = Some(CachedSyncIssue::Conflict);
        save_metadata(&metadata_path, &metadata).unwrap();
        assert_eq!(
            asset_sync_state(&config, &root, &asset),
            CloudSyncState::Conflict
        );
        assert_eq!(
            cached_asset_sync_state(&raw_path),
            Some((asset.id.clone(), CloudSyncState::Conflict))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn local_original_filename_is_not_treated_as_a_cloud_cache() {
        let directory = test_directory("local-original");
        let raw_path = directory.join("original.dng");
        fs::create_dir_all(&directory).unwrap();
        fs::write(&raw_path, b"raw").unwrap();
        fs::write(
            crate::sidecar::sidecar_path_for_raw(&raw_path),
            b"local-sidecar",
        )
        .unwrap();
        save_metadata(
            &metadata_path_for_directory(&directory),
            &CachedAssetMetadata {
                schema_version: 1,
                asset_id: "a".repeat(64),
                server_url: "http://cloud.test:8787".to_owned(),
                access_token: "test-token".to_owned(),
                raw_etag: "b".repeat(64),
                sidecar_etag: None,
                thumbnail_etag: "c".repeat(64),
                pending_sidecar_upload: false,
                sync_issue: None,
            },
        )
        .unwrap();

        assert_eq!(sync_sidecar_if_cloud_cached(&raw_path, false).unwrap(), None);
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn conflict_resolution_can_overwrite_server_with_preserved_local_sidecar() {
        let raw = b"conflicted-raw";
        let local_sidecar = b"preserved-local-sidecar";
        let server_sidecar = b"newer-server-sidecar";
        let local_etag = digest_bytes(local_sidecar);
        let server_etag = digest_bytes(server_sidecar);
        let asset = test_asset(raw, Some(server_etag.clone()));
        let catalog = serde_json::to_vec(&CloudCatalog {
            items: vec![asset.clone()],
            folders: Vec::new(),
        })
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected_asset_id = asset.id.clone();
        let expected_server_etag = server_etag.clone();
        let response_local_etag = local_etag.clone();
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_test_http_request(&mut stream);
            assert!(
                String::from_utf8_lossy(&request).starts_with("GET /api/v1/assets HTTP/1.1\r\n")
            );
            write_test_http_response(&mut stream, "200 OK", "application/json", "", &catalog);

            let (mut stream, _) = listener.accept().unwrap();
            let request = read_test_http_request(&mut stream);
            let request_text = String::from_utf8_lossy(&request);
            assert!(request_text.starts_with(&format!(
                "PUT /api/v1/assets/{expected_asset_id}/sidecar HTTP/1.1\r\n"
            )));
            assert!(request_text.lines().any(|line| {
                line.eq_ignore_ascii_case(&format!("If-Match: \"{expected_server_etag}\""))
            }));
            assert!(request.ends_with(local_sidecar));
            write_test_http_response(
                &mut stream,
                "204 No Content",
                "application/octet-stream",
                &format!("ETag: \"{response_local_etag}\"\r\n"),
                &[],
            );
        });

        let root = test_directory("overwrite-server-conflict");
        let directory = root.join("f".repeat(64)).join(&asset.id);
        fs::create_dir_all(&directory).unwrap();
        let raw_path = directory.join("original.dng");
        fs::write(&raw_path, raw).unwrap();
        fs::write(
            crate::sidecar::sidecar_path_for_raw(&raw_path),
            local_sidecar,
        )
        .unwrap();
        let metadata_path = metadata_path_for_directory(&directory);
        save_metadata(
            &metadata_path,
            &CachedAssetMetadata {
                schema_version: 1,
                asset_id: asset.id,
                server_url: format!("http://{address}"),
                access_token: String::new(),
                raw_etag: asset.raw_etag,
                sidecar_etag: Some("e".repeat(64)),
                thumbnail_etag: asset.thumbnail_etag,
                pending_sidecar_upload: true,
                sync_issue: None,
            },
        )
        .unwrap();

        overwrite_server_sidecar_with_local(&raw_path).unwrap();
        responder.join().unwrap();
        let metadata = load_metadata(&metadata_path).unwrap().unwrap();
        assert_eq!(metadata.sidecar_etag, Some(local_etag));
        assert!(!metadata.pending_sidecar_upload);
        assert_eq!(
            fs::read(crate::sidecar::sidecar_path_for_raw(&raw_path)).unwrap(),
            local_sidecar
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn conflict_resolution_can_overwrite_local_sidecar_with_server() {
        let raw = b"conflicted-raw";
        let local_sidecar = b"preserved-local-sidecar";
        let server_sidecar = b"authoritative-server-sidecar";
        let server_etag = digest_bytes(server_sidecar);
        let asset = test_asset(raw, Some(server_etag.clone()));
        let catalog = serde_json::to_vec(&CloudCatalog {
            items: vec![asset.clone()],
            folders: Vec::new(),
        })
        .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected_sidecar_path = format!("/api/v1/assets/{}/sidecar", asset.id);
        let responder = std::thread::spawn(move || {
            for (expected_path, content_type, body) in [
                (
                    "/api/v1/assets".to_owned(),
                    "application/json",
                    catalog.clone(),
                ),
                (
                    expected_sidecar_path,
                    "application/vnd.auraw.sidecar",
                    server_sidecar.to_vec(),
                ),
                ("/api/v1/assets".to_owned(), "application/json", catalog),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                assert!(String::from_utf8_lossy(&request)
                    .starts_with(&format!("GET {expected_path} HTTP/1.1\r\n")));
                write_test_http_response(&mut stream, "200 OK", content_type, "", &body);
            }
        });

        let root = test_directory("overwrite-local-conflict");
        let directory = root.join("f".repeat(64)).join(&asset.id);
        fs::create_dir_all(&directory).unwrap();
        let raw_path = directory.join("original.dng");
        fs::write(&raw_path, raw).unwrap();
        fs::write(
            crate::sidecar::sidecar_path_for_raw(&raw_path),
            local_sidecar,
        )
        .unwrap();
        let metadata_path = metadata_path_for_directory(&directory);
        save_metadata(
            &metadata_path,
            &CachedAssetMetadata {
                schema_version: 1,
                asset_id: asset.id,
                server_url: format!("http://{address}"),
                access_token: String::new(),
                raw_etag: asset.raw_etag,
                sidecar_etag: Some("e".repeat(64)),
                thumbnail_etag: asset.thumbnail_etag,
                pending_sidecar_upload: true,
                sync_issue: None,
            },
        )
        .unwrap();

        overwrite_local_sidecar_with_server(&raw_path).unwrap();
        responder.join().unwrap();
        let metadata = load_metadata(&metadata_path).unwrap().unwrap();
        assert_eq!(metadata.sidecar_etag, Some(server_etag));
        assert!(!metadata.pending_sidecar_upload);
        assert_eq!(
            fs::read(crate::sidecar::sidecar_path_for_raw(&raw_path)).unwrap(),
            server_sidecar
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn opening_a_stale_card_fetches_the_current_sidecar_version() {
        let raw = b"fresh-raw";
        let sidecar = b"fresh-sidecar";
        let fresh_etag = digest_bytes(sidecar);
        let fresh_asset = test_asset(raw, Some(fresh_etag));
        let mut stale_asset = fresh_asset.clone();
        stale_asset.sidecar_etag = Some("e".repeat(64));
        let catalog = serde_json::to_vec(&CloudCatalog {
            items: vec![fresh_asset.clone()],
            folders: Vec::new(),
        })
        .unwrap();

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let responder = std::thread::spawn(move || {
            for (expected_path, content_type, body) in [
                ("/api/v1/assets".to_owned(), "application/json", catalog),
                (
                    format!("/api/v1/assets/{}/raw", fresh_asset.id),
                    "application/octet-stream",
                    raw.to_vec(),
                ),
                (
                    format!("/api/v1/assets/{}/sidecar", fresh_asset.id),
                    "application/vnd.auraw.sidecar",
                    sidecar.to_vec(),
                ),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut request = Vec::new();
                let mut buffer = [0u8; 2048];
                while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
                    let count = stream.read(&mut buffer).unwrap();
                    assert!(count > 0);
                    request.extend_from_slice(&buffer[..count]);
                }
                let first_line = String::from_utf8_lossy(&request)
                    .lines()
                    .next()
                    .unwrap()
                    .to_owned();
                assert_eq!(first_line, format!("GET {expected_path} HTTP/1.1"));
                write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                stream.write_all(&body).unwrap();
            }
        });

        let directory = test_directory("stale-click");
        let config = CloudConfig {
            enabled: true,
            server_url: format!("http://{address}"),
            access_token: String::new(),
        };
        let cached = open_asset(&config, &directory, &stale_asset, true, |_, _| {}).unwrap();
        responder.join().unwrap();
        assert_eq!(
            fs::read(crate::sidecar::sidecar_path_for_raw(&cached.raw_path)).unwrap(),
            sidecar
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn desktop_upload_streams_raw_and_matching_sidecar_as_multipart() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let responder = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let header_end = loop {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0, "client closed before sending HTTP headers");
                request.extend_from_slice(&buffer[..count]);
                if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                    break index + 4;
                }
            };
            let content_length = {
                let headers = String::from_utf8_lossy(&request[..header_end]);
                headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .expect("file-backed multipart form should have a content length")
            };
            while request.len() < header_end + content_length {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0, "client closed before sending the multipart body");
                request.extend_from_slice(&buffer[..count]);
            }

            let headers = String::from_utf8_lossy(&request[..header_end]);
            assert!(headers.starts_with("POST /api/v1/assets HTTP/1.1\r\n"));
            assert!(headers
                .lines()
                .any(|line| line.eq_ignore_ascii_case("authorization: Bearer test-token")));
            assert!(headers.to_ascii_lowercase().contains("multipart/form-data"));
            let body = String::from_utf8_lossy(&request[header_end..header_end + content_length]);
            assert!(body.contains("name=\"raw\""));
            assert!(body.contains("filename=\"upload-test.dng\""));
            assert!(body.contains("raw-upload-body"));
            assert!(body.contains("name=\"sidecar\""));
            assert!(body.contains("sidecar-upload-body"));
            assert!(body.contains("name=\"folder_id\""));
            assert!(body.contains(CLOUD_ROOT_FOLDER_ID));

            let response_body = format!(
                "{{\"id\":\"{}\",\"name\":\"upload-test.dng\",\"bytes\":15,\"modified_seconds\":1,\"width\":512,\"height\":341,\"raw_etag\":\"{}\",\"sidecar_etag\":\"{}\",\"thumbnail_etag\":\"{}\"}}",
                "a".repeat(64),
                "b".repeat(64),
                "c".repeat(64),
                "d".repeat(64),
            );
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response_body.len(),
                response_body,
            )
            .unwrap();
        });

        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "auraw-cloud-upload-test-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        let raw_path = directory.join("upload-test.dng");
        fs::write(&raw_path, b"raw-upload-body").unwrap();
        fs::write(
            crate::sidecar::sidecar_path_for_raw(&raw_path),
            b"sidecar-upload-body",
        )
        .unwrap();
        let asset = upload_asset_path(
            &CloudConfig {
                enabled: true,
                server_url: format!("http://{address}"),
                access_token: "test-token".to_owned(),
            },
            &raw_path,
        )
        .unwrap();
        responder.join().unwrap();
        fs::remove_dir_all(directory).unwrap();

        assert_eq!(asset.name, "upload-test.dng");
        assert_eq!(asset.sidecar_etag, Some("c".repeat(64)));
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn trash_api_lists_restores_and_permanently_deletes_items() {
        let trash_id = "a".repeat(64);
        let catalog = format!(
            "{{\"items\":[{{\"id\":\"{trash_id}\",\"kind\":\"asset\",\"name\":\"deleted.dng\",\"deleted_seconds\":100,\"expires_seconds\":200,\"bytes\":42,\"item_count\":1}}],\"server_time\":120,\"retention_days\":14}}"
        );
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let expected_id = trash_id.clone();
        let responder = std::thread::spawn(move || {
            for (method, path, status, body) in [
                ("GET", "/api/v1/trash".to_owned(), "200 OK", catalog.into_bytes()),
                (
                    "POST",
                    format!("/api/v1/trash/{expected_id}/restore"),
                    "200 OK",
                    b"{\"kind\":\"asset\",\"name\":\"deleted.dng\"}".to_vec(),
                ),
                (
                    "DELETE",
                    format!("/api/v1/trash/{expected_id}"),
                    "204 No Content",
                    Vec::new(),
                ),
                ("DELETE", "/api/v1/trash".to_owned(), "204 No Content", Vec::new()),
            ] {
                let (mut stream, _) = listener.accept().unwrap();
                let request = read_test_http_request(&mut stream);
                assert!(String::from_utf8_lossy(&request)
                    .starts_with(&format!("{method} {path} HTTP/1.1\r\n")));
                write_test_http_response(
                    &mut stream,
                    status,
                    "application/json",
                    "",
                    &body,
                );
            }
        });
        let config = CloudConfig {
            enabled: true,
            server_url: format!("http://{address}"),
            access_token: String::new(),
        };
        let trash = list_trash(&config).unwrap();
        assert_eq!(trash.items.len(), 1);
        assert_eq!(trash.retention_days, 14);
        let item = trash.items[0].clone();
        assert_eq!(restore_trash_item(&config, &item, None).unwrap(), "deleted.dng");
        permanently_delete_trash_item(&config, &item).unwrap();
        empty_trash(&config).unwrap();
        responder.join().unwrap();
    }

    #[test]
    #[ignore = "requires AURAW_CLOUD_TEST_URL and a running AuRaw Cloud server"]
    fn reader_upload_round_trips_against_configured_cloud_server() {
        let server_url = std::env::var("AURAW_CLOUD_TEST_URL")
            .expect("set AURAW_CLOUD_TEST_URL for the live cloud integration test");
        let access_token = std::env::var("AURAW_CLOUD_TEST_TOKEN").unwrap_or_default();
        let raw_path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../regression/raw/synthetic-xtrans.dng");
        let bytes = fs::metadata(&raw_path).unwrap().len();
        let raw = File::open(&raw_path).unwrap();

        let asset = upload_asset_file(
            &CloudConfig {
                enabled: true,
                server_url,
                access_token,
            },
            raw,
            "android-reader-upload.dng",
            Some(bytes),
        )
        .unwrap();

        assert_eq!(asset.name, "android-reader-upload.dng");
        assert_eq!(asset.bytes, bytes);
    }
}
