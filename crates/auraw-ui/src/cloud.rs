use crate::pipeline::RawThumbnail;
use ring::digest::{Context as Sha256Context, SHA256};
use serde::{Deserialize, Serialize};
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
}

#[derive(Deserialize)]
struct CloudCatalog {
    items: Vec<CloudAsset>,
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
}

#[derive(Clone, Debug)]
pub struct CachedCloudAsset {
    pub raw_path: PathBuf,
    pub label: String,
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

fn validate_asset(asset: &CloudAsset) -> Result<(), String> {
    validate_hex_identifier(&asset.id, "asset ID")?;
    validate_hex_identifier(&asset.raw_etag, "RAW version")?;
    validate_hex_identifier(&asset.thumbnail_etag, "thumbnail version")?;
    if let Some(etag) = &asset.sidecar_etag {
        validate_hex_identifier(etag, "sidecar version")?;
    }
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

pub fn list_assets(config: &CloudConfig) -> Result<Vec<CloudAsset>, String> {
    let normalized = config.normalized()?;
    let url = normalized.endpoint("/api/v1/assets")?;
    let mut response = get(&normalized, &url).map_err(|error| match error {
        ureq::Error::StatusCode(401) => "AuRaw Cloud rejected the access token.".to_owned(),
        _ => format!("Could not load the cloud catalog: {error}"),
    })?;
    let bytes = response
        .body_mut()
        .with_config()
        .limit(MAX_CATALOG_BYTES)
        .read_to_vec()
        .map_err(|error| format!("Could not read the cloud catalog: {error}"))?;
    let catalog: CloudCatalog = serde_json::from_slice(&bytes)
        .map_err(|error| format!("Cloud returned an invalid catalog: {error}"))?;
    if catalog.items.len() > MAX_CATALOG_ASSETS {
        return Err(format!(
            "Cloud returned more than {MAX_CATALOG_ASSETS} catalog entries."
        ));
    }
    for asset in &catalog.items {
        validate_asset(asset)?;
    }
    Ok(catalog.items)
}

pub fn test_connection(config: &CloudConfig) -> Result<String, String> {
    let assets = list_assets(config)?;
    Ok(format!(
        "Connected to AuRaw Cloud · {} {}",
        assets.len(),
        if assets.len() == 1 { "photo" } else { "photos" }
    ))
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

#[cfg(not(target_os = "android"))]
fn checked_upload_part(
    path: &Path,
    maximum_bytes: u64,
    label: &str,
) -> Result<ureq::unversioned::multipart::Part<'static>, String> {
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

fn send_upload_form(
    config: &CloudConfig,
    form: ureq::unversioned::multipart::Form<'_>,
) -> Result<CloudAsset, String> {
    let config = config.normalized()?;
    let url = config.endpoint("/api/v1/assets")?;
    let request = agent().post(&url);
    let request = if let Some(value) = authorization(&config) {
        request.header("Authorization", value)
    } else {
        request
    };
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
    send_upload_form(config, form)
}

pub fn upload_asset_file(
    config: &CloudConfig,
    raw: File,
    display_name: &str,
    declared_bytes: Option<u64>,
) -> Result<CloudAsset, String> {
    validate_upload_name(display_name)?;
    validate_upload_size(display_name, declared_bytes)?;
    let raw = ureq::unversioned::multipart::Part::owned_reader(raw.take(MAX_RAW_BYTES + 1))
        .file_name(display_name)
        .mime_str("application/octet-stream")
        .map_err(|error| format!("Could not prepare {display_name} for upload: {error}"))?;
    send_upload_form(
        config,
        ureq::unversioned::multipart::Form::new().part("raw", raw),
    )
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
    raw_path.parent().map(metadata_path_for_directory)
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

fn request_sidecar_upload(
    metadata: &mut CachedAssetMetadata,
    sidecar_path: &Path,
) -> Result<(), String> {
    let bytes = crate::sidecar::read_bounded(sidecar_path).map_err(|error| error.to_string())?;
    let config = config_from_metadata(metadata);
    let url = config.endpoint(&format!("/api/v1/assets/{}/sidecar", metadata.asset_id))?;
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
    let response = request.send(&bytes).map_err(|error| match error {
        ureq::Error::StatusCode(412) => {
            "Cloud edit conflict: another client saved this image first. Your local cached sidecar was preserved and remains marked as waiting; the server copy was not overwritten."
                .to_owned()
        }
        ureq::Error::StatusCode(401) => "AuRaw Cloud rejected the access token.".to_owned(),
        _ => format!("The sidecar was saved locally but could not sync to AuRaw Cloud: {error}"),
    })?;
    let etag = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_matches('"').to_owned())
        .ok_or_else(|| "AuRaw Cloud saved the sidecar without returning its version.".to_owned())?;
    validate_hex_identifier(&etag, "sidecar version")?;
    metadata.sidecar_etag = Some(etag);
    metadata.pending_sidecar_upload = false;
    Ok(())
}

pub fn sync_sidecar_if_cloud_cached(raw_path: &Path) -> Result<Option<String>, String> {
    let Some(metadata_path) = metadata_path_for_raw(raw_path) else {
        return Ok(None);
    };
    let Some(mut metadata) = load_metadata(&metadata_path)? else {
        return Ok(None);
    };
    let sidecar_path = crate::sidecar::sidecar_path_for_raw(raw_path);
    metadata.pending_sidecar_upload = true;
    save_metadata(&metadata_path, &metadata)?;
    request_sidecar_upload(&mut metadata, &sidecar_path)?;
    save_metadata(&metadata_path, &metadata)?;
    Ok(Some(format!("AuRaw Cloud ({})", metadata.server_url)))
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
            log::warn!("cloud sidecar remains pending while reopening the cache: {error}");
        }
        save_metadata(metadata_path, metadata)?;
        return Ok(());
    }
    if metadata.pending_sidecar_upload {
        // The pending recovery file was removed outside AuRaw, so there is no
        // local edit left to upload. Resume normal remote-sidecar refresh.
        metadata.pending_sidecar_upload = false;
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
        raw_path,
        label: asset.name.clone(),
    })
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
    let response = request.send(&jpeg).map_err(|error| match error {
        ureq::Error::StatusCode(412) => {
            "The developed cloud thumbnail belongs to an older sidecar revision.".to_owned()
        }
        _ => format!("Could not sync the developed cloud thumbnail: {error}"),
    })?;
    if let Some(etag) = response
        .headers()
        .get("etag")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.trim_matches('"').to_owned())
    {
        validate_hex_identifier(&etag, "thumbnail version")?;
        metadata.thumbnail_etag = etag;
        save_metadata(&metadata_path, &metadata)?;
    }
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
    Some(if metadata.pending_sidecar_upload {
        "Cloud · waiting to sync"
    } else {
        "Cloud · synced"
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
