use anyhow::{Context, Result};
use auraw_core::file_ops::{replace_file, sync_parent_directory};
use ring::digest::{Context as Sha256Context, SHA256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const IO_BUFFER_BYTES: usize = 256 * 1024;
static NEXT_PARTIAL_ARTIFACT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactSize {
    Exact(u64),
    Max(u64),
}

impl ArtifactSize {
    fn limit(self) -> u64 {
        match self {
            Self::Exact(bytes) | Self::Max(bytes) => bytes,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct ModelArtifact {
    pub name: &'static str,
    pub url: Option<&'static str>,
    pub sha256: &'static str,
    pub size: ArtifactSize,
    pub progress_total: u64,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct DownloadOptions {
    pub connect_timeout: Duration,
    pub response_timeout: Duration,
    pub body_timeout: Duration,
    pub attempts: usize,
    pub resume: bool,
}

pub(crate) fn ensure_artifact<F, C>(
    path: &Path,
    artifact: ModelArtifact,
    options: DownloadOptions,
    progress: F,
    cancellation: C,
) -> Result<()>
where
    F: FnMut(u64, u64),
    C: FnMut() -> Result<()>,
{
    match verify_artifact(path, artifact) {
        Ok(()) => return Ok(()),
        Err(error) if path.exists() => {
            log::warn!(
                "replacing invalid {} cache {}: {error:#}",
                artifact.name,
                path.display()
            );
        }
        Err(_) => {}
    }

    download_artifact(path, artifact, options, progress, cancellation)?;
    verify_artifact(path, artifact).with_context(|| format!("verify published {}", artifact.name))
}

pub(crate) fn install_artifact_from_reader<R, C>(
    path: &Path,
    artifact: ModelArtifact,
    reader: &mut R,
    mut cancellation: C,
) -> Result<()>
where
    R: Read,
    C: FnMut() -> Result<()>,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create model cache {}", parent.display()))?;
    }
    let partial = PartialArtifact::new(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(partial.path())
        .with_context(|| format!("create {}", partial.path().display()))?;
    let downloaded = copy_download(
        reader,
        &mut file,
        0,
        artifact.size.limit(),
        artifact.progress_total,
        artifact.name,
        &mut |_, _| {},
        &mut cancellation,
    )
    .map_err(TransferError::into_anyhow)?;
    file.sync_all()
        .with_context(|| format!("flush {}", artifact.name))?;
    if let ArtifactSize::Exact(expected) = artifact.size {
        anyhow::ensure!(
            downloaded == expected,
            "{} size mismatch: received {downloaded}, expected {expected}",
            artifact.name
        );
    }
    verify_artifact(partial.path(), artifact)?;
    cancellation()?;
    partial
        .publish(path)
        .with_context(|| format!("publish {} to {}", artifact.name, path.display()))
}

pub(crate) fn verify_artifact(path: &Path, artifact: ModelArtifact) -> Result<()> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("read {} metadata {}", artifact.name, path.display()))?;
    anyhow::ensure!(
        metadata.is_file(),
        "{} cache is not a regular file",
        artifact.name
    );
    match artifact.size {
        ArtifactSize::Exact(expected) => anyhow::ensure!(
            metadata.len() == expected,
            "{} size mismatch: found {}, expected {expected}",
            artifact.name,
            metadata.len()
        ),
        ArtifactSize::Max(maximum) => anyhow::ensure!(
            metadata.len() > 0 && metadata.len() <= maximum,
            "{} size {} is outside the allowed range 1..={maximum}",
            artifact.name,
            metadata.len()
        ),
    }
    let actual = sha256_file_hex(path)?;
    anyhow::ensure!(
        actual == artifact.sha256,
        "{} SHA-256 mismatch (expected {})",
        artifact.name,
        artifact.sha256
    );
    Ok(())
}

pub fn sha256_file_hex(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256Context::new(&SHA256);
    let mut buffer = vec![0u8; IO_BUFFER_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("hash {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finish().as_ref()))
}

#[cfg(not(target_os = "android"))]
pub fn desktop_model_cache_root() -> PathBuf {
    let root = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
        .unwrap_or_else(std::env::temp_dir);
    root.join("auraw/models")
}

fn download_artifact<F, C>(
    path: &Path,
    artifact: ModelArtifact,
    options: DownloadOptions,
    mut progress: F,
    mut cancellation: C,
) -> Result<()>
where
    F: FnMut(u64, u64),
    C: FnMut() -> Result<()>,
{
    let url = artifact
        .url
        .with_context(|| format!("{} has no download URL", artifact.name))?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create model cache {}", parent.display()))?;
    }

    let partial = PartialArtifact::new(path);
    let attempts = options.attempts.max(1);
    let config = ureq::Agent::config_builder()
        .https_only(true)
        .timeout_connect(Some(options.connect_timeout))
        .timeout_recv_response(Some(options.response_timeout))
        .timeout_recv_body(Some(options.body_timeout))
        .build();
    let agent: ureq::Agent = config.into();
    let mut last_error: Option<anyhow::Error> = None;

    for attempt in 0..attempts {
        cancellation()?;
        let mut downloaded = if options.resume {
            fs::metadata(partial.path())
                .map(|metadata| metadata.len())
                .unwrap_or(0)
        } else {
            0
        };
        if downloaded > artifact.size.limit() {
            partial
                .remove()
                .with_context(|| format!("remove oversized partial {} download", artifact.name))?;
            downloaded = 0;
        }
        if downloaded > 0 {
            if verify_artifact(partial.path(), artifact).is_ok() {
                cancellation()?;
                partial.publish(path).with_context(|| {
                    format!("publish resumed {} to {}", artifact.name, path.display())
                })?;
                return Ok(());
            }
            if matches!(artifact.size, ArtifactSize::Exact(expected) if downloaded == expected) {
                partial.remove().with_context(|| {
                    format!("remove corrupt complete partial {} download", artifact.name)
                })?;
                downloaded = 0;
            }
        }
        if downloaded > 0 {
            progress(downloaded, artifact.progress_total.max(downloaded));
        }

        let response_result = if options.resume && downloaded > 0 {
            let range = format!("bytes={downloaded}-");
            agent.get(url).header("Range", range.as_str()).call()
        } else {
            agent.get(url).call()
        };
        let mut response = match response_result {
            Ok(response) => response,
            Err(error) => {
                last_error = Some(anyhow::Error::new(error).context(format!(
                    "download {} (attempt {}/{attempts})",
                    artifact.name,
                    attempt + 1
                )));
                if attempt + 1 < attempts {
                    retry_backoff(attempt, &mut cancellation)?;
                    continue;
                }
                break;
            }
        };

        let resuming = options.resume && downloaded > 0 && response.status().as_u16() == 206;
        if downloaded > 0 && !resuming {
            downloaded = 0;
        }
        let declared_remaining = response.body().content_length();
        let declared_total = match declared_remaining {
            Some(length) if resuming => Some(
                downloaded
                    .checked_add(length)
                    .context("model response length overflow")?,
            ),
            Some(length) => Some(length),
            None => None,
        };
        match (artifact.size, declared_total) {
            (ArtifactSize::Exact(expected), Some(total)) => anyhow::ensure!(
                total == expected,
                "{} server declared {total} total bytes, expected {expected}",
                artifact.name
            ),
            (ArtifactSize::Max(maximum), Some(total)) => anyhow::ensure!(
                total <= maximum,
                "{} response declares {total} bytes, above the {maximum}-byte limit",
                artifact.name
            ),
            _ => {}
        }
        let total = declared_total.unwrap_or_else(|| artifact.progress_total.max(downloaded));

        let partial_exists = partial.path().exists();
        let mut open = OpenOptions::new();
        open.write(true);
        if resuming {
            open.append(true);
        } else if partial_exists {
            open.truncate(true);
        } else {
            open.create_new(true);
        }
        let mut file = open.open(partial.path()).with_context(|| {
            format!(
                "open partial {} download {}",
                artifact.name,
                partial.path().display()
            )
        })?;
        let mut reader = response.body_mut().as_reader();
        let transfer = copy_download(
            &mut reader,
            &mut file,
            downloaded,
            artifact.size.limit(),
            total,
            artifact.name,
            &mut progress,
            &mut cancellation,
        );
        downloaded = match transfer {
            Ok(downloaded) => downloaded,
            Err(TransferError::Fatal(error)) => return Err(error),
            Err(TransferError::Retry(error)) => {
                let _ = file.sync_data();
                last_error = Some(error.context(format!(
                    "read {} download (attempt {}/{attempts})",
                    artifact.name,
                    attempt + 1
                )));
                if attempt + 1 < attempts {
                    retry_backoff(attempt, &mut cancellation)?;
                    continue;
                }
                break;
            }
        };

        file.sync_all()
            .with_context(|| format!("flush {}", artifact.name))?;
        if let ArtifactSize::Exact(expected) = artifact.size {
            if downloaded < expected {
                last_error = Some(anyhow::anyhow!(
                    "{} download ended early at {downloaded} / {expected} bytes",
                    artifact.name
                ));
                if attempt + 1 < attempts {
                    retry_backoff(attempt, &mut cancellation)?;
                    continue;
                }
                break;
            }
        }

        match verify_artifact(partial.path(), artifact) {
            Ok(()) => {
                cancellation()?;
                partial
                    .publish(path)
                    .with_context(|| format!("publish {} to {}", artifact.name, path.display()))?;
                return Ok(());
            }
            Err(error) => {
                last_error = Some(error);
                if matches!(artifact.size, ArtifactSize::Exact(_)) {
                    partial.remove().with_context(|| {
                        format!("remove corrupt partial {} download", artifact.name)
                    })?;
                }
                if attempt + 1 < attempts {
                    retry_backoff(attempt, &mut cancellation)?;
                }
            }
        }
    }

    Err(last_error.unwrap_or_else(|| anyhow::anyhow!("download {} failed", artifact.name)))
}

enum TransferError {
    Retry(anyhow::Error),
    Fatal(anyhow::Error),
}

impl TransferError {
    fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Retry(error) | Self::Fatal(error) => error,
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn copy_download<R, W, F, C>(
    reader: &mut R,
    writer: &mut W,
    mut downloaded: u64,
    max_bytes: u64,
    total: u64,
    name: &str,
    progress: &mut F,
    cancellation: &mut C,
) -> std::result::Result<u64, TransferError>
where
    R: Read,
    W: Write,
    F: FnMut(u64, u64),
    C: FnMut() -> Result<()>,
{
    let mut buffer = vec![0u8; IO_BUFFER_BYTES];
    loop {
        cancellation().map_err(TransferError::Fatal)?;
        let read = reader
            .read(&mut buffer)
            .map_err(|error| TransferError::Retry(anyhow::Error::new(error)))?;
        if read == 0 {
            return Ok(downloaded);
        }
        downloaded = downloaded
            .checked_add(read as u64)
            .context("model download byte count overflow")
            .map_err(TransferError::Fatal)?;
        if downloaded > max_bytes {
            return Err(TransferError::Fatal(anyhow::anyhow!(
                "{name} download exceeded the {max_bytes}-byte limit"
            )));
        }
        writer
            .write_all(&buffer[..read])
            .with_context(|| format!("write {name}"))
            .map_err(TransferError::Fatal)?;
        progress(downloaded, total.max(downloaded));
    }
}

fn retry_backoff<C>(attempt: usize, cancellation: &mut C) -> Result<()>
where
    C: FnMut() -> Result<()>,
{
    let mut remaining = Duration::from_secs(1u64 << attempt.min(3));
    while !remaining.is_zero() {
        cancellation()?;
        let sleep_for = remaining.min(Duration::from_millis(100));
        std::thread::sleep(sleep_for);
        remaining = remaining.saturating_sub(sleep_for);
    }
    cancellation()
}

struct PartialArtifact(PathBuf);

impl PartialArtifact {
    fn new(destination: &Path) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let extension = destination
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("model");
        let temporary_id = NEXT_PARTIAL_ARTIFACT_ID.fetch_add(1, Ordering::Relaxed);
        Self(destination.with_extension(format!(
            "{extension}.{}.{}.{}.part",
            std::process::id(),
            nonce,
            temporary_id
        )))
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn remove(&self) -> std::io::Result<()> {
        match fs::remove_file(&self.0) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn publish(&self, destination: &Path) -> std::io::Result<()> {
        replace_file(&self.0, destination)?;
        let parent = destination
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        sync_parent_directory(parent)
    }
}

impl Drop for PartialArtifact {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ARTIFACT: ModelArtifact = ModelArtifact {
        name: "test model",
        url: None,
        sha256: "7702832f291b1ad6d8269d712184a9ddc87c9bac3833fa10b3f2140830fb4c47",
        size: ArtifactSize::Exact(17),
        progress_total: 17,
    };

    fn write_temp(bytes: &[u8]) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model.onnx");
        fs::write(&path, bytes).unwrap();
        (temp, path)
    }

    #[test]
    fn valid_cached_artifact_is_accepted() {
        let (_temp, path) = write_temp(b"valid model bytes");
        verify_artifact(&path, TEST_ARTIFACT).unwrap();
    }

    #[test]
    fn checksum_mismatch_is_rejected() {
        let (_temp, path) = write_temp(b"invalid model byt");
        let error = verify_artifact(&path, TEST_ARTIFACT).unwrap_err();
        assert!(format!("{error:#}").contains("SHA-256 mismatch"));
    }

    #[test]
    fn partial_artifact_is_rejected() {
        let (_temp, path) = write_temp(b"partial");
        let error = verify_artifact(&path, TEST_ARTIFACT).unwrap_err();
        assert!(format!("{error:#}").contains("size mismatch"));
    }

    #[test]
    fn failed_install_cleans_partial_file() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("model.onnx");
        let mut source = std::io::Cursor::new(b"invalid model byt");
        assert!(
            install_artifact_from_reader(&destination, TEST_ARTIFACT, &mut source, || Ok(()))
                .is_err()
        );
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }

    #[test]
    fn successful_install_replaces_existing_file_atomically_and_is_verified() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("model.onnx");
        fs::write(&destination, b"old invalid model").unwrap();
        let mut source = std::io::Cursor::new(b"valid model bytes");
        install_artifact_from_reader(&destination, TEST_ARTIFACT, &mut source, || Ok(())).unwrap();
        verify_artifact(&destination, TEST_ARTIFACT).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), b"valid model bytes");
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 1);
    }

    #[test]
    fn retry_backoff_observes_cancellation_before_sleeping() {
        let mut checks = 0;
        let error = retry_backoff(3, &mut || {
            checks += 1;
            anyhow::bail!("background task cancelled")
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("cancelled"));
        assert_eq!(checks, 1);
    }

    #[test]
    fn cancellation_does_not_publish_verified_partial() {
        let temp = tempfile::tempdir().unwrap();
        let destination = temp.path().join("model.onnx");
        let mut source = std::io::Cursor::new(b"valid model bytes");
        let mut checks = 0;
        let error = install_artifact_from_reader(&destination, TEST_ARTIFACT, &mut source, || {
            checks += 1;
            anyhow::ensure!(checks < 2, "background task cancelled");
            Ok(())
        })
        .unwrap_err();
        assert!(format!("{error:#}").contains("cancelled"));
        assert!(!destination.exists());
        assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
    }
}
