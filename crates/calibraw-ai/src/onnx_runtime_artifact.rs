use crate::model_artifact::{
    ensure_artifact, sha256_file_hex, ArtifactSize, DownloadOptions, ModelArtifact,
};
use anyhow::{Context, Result};
use flate2::read::GzDecoder;
use std::{
    fs::{self, File},
    path::{Component, Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
    time::Duration,
};

const INSTALL_MANIFEST: &str = "calibraw-runtime.txt";

#[derive(Clone, Copy)]
enum ArchiveFormat {
    TarGz,
    Zip,
}

#[derive(Clone, Copy)]
struct RuntimePackage {
    platform: &'static str,
    version: &'static str,
    archive_name: &'static str,
    url: &'static str,
    bytes: u64,
    sha256: &'static str,
    format: ArchiveFormat,
}

fn runtime_package() -> Result<RuntimePackage> {
    let package = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => RuntimePackage {
            platform: "linux-x86_64",
            version: "1.29.0",
            archive_name: "onnxruntime-linux-x64-1.29.0.tgz",
            url: "https://huggingface.co/Duecki/CalibRaw-Artifacts/resolve/91085ce0ec322a4a7cbd20059688690218e52f9a/onnxruntime/linux-x86_64/onnxruntime-linux-x64-1.29.0.tgz",
            bytes: 11_082_880,
            sha256: "c3fddc4f139a045b0c4902c57410f0694f1c2fdf9b6939fbe38b1aeae7cd14ba",
            format: ArchiveFormat::TarGz,
        },
        ("linux", "aarch64") => RuntimePackage {
            platform: "linux-arm64",
            version: "1.29.0",
            archive_name: "onnxruntime-linux-aarch64-1.29.0.tgz",
            url: "https://huggingface.co/Duecki/CalibRaw-Artifacts/resolve/91085ce0ec322a4a7cbd20059688690218e52f9a/onnxruntime/linux-arm64/onnxruntime-linux-aarch64-1.29.0.tgz",
            bytes: 10_027_600,
            sha256: "e1799098ebc054b370f6176a450f158720f297818c613e5dc99b92e2ec82346f",
            format: ArchiveFormat::TarGz,
        },
        ("macos", "aarch64") => RuntimePackage {
            platform: "macos-arm64",
            version: "1.29.0",
            archive_name: "onnxruntime-osx-arm64-1.29.0.tgz",
            url: "https://huggingface.co/Duecki/CalibRaw-Artifacts/resolve/91085ce0ec322a4a7cbd20059688690218e52f9a/onnxruntime/macos-arm64/onnxruntime-osx-arm64-1.29.0.tgz",
            bytes: 41_578_864,
            sha256: "d0706fc34f315d8c88639d0a8c81f2e09e815f282cabed3493c06a054352cf92",
            format: ArchiveFormat::TarGz,
        },
        ("macos", "x86_64") => RuntimePackage {
            platform: "macos-x86_64",
            version: "1.23.2",
            archive_name: "onnxruntime-osx-x86_64-1.23.2.tgz",
            url: "https://huggingface.co/Duecki/CalibRaw-Artifacts/resolve/91085ce0ec322a4a7cbd20059688690218e52f9a/onnxruntime/macos-x86_64/onnxruntime-osx-x86_64-1.23.2.tgz",
            bytes: 11_676_322,
            sha256: "d10359e16347b57d9959f7e80a225a5b4a66ed7d7e007274a15cae86836485a6",
            format: ArchiveFormat::TarGz,
        },
        ("windows", "x86_64") => RuntimePackage {
            platform: "windows-x86_64",
            version: "1.29.0",
            archive_name: "onnxruntime-win-x64-1.29.0.zip",
            url: "https://huggingface.co/Duecki/CalibRaw-Artifacts/resolve/91085ce0ec322a4a7cbd20059688690218e52f9a/onnxruntime/windows-x86_64/onnxruntime-win-x64-1.29.0.zip",
            bytes: 79_645_520,
            sha256: "c9b4b7086b529ad814f428c1bad028e20a25d7dc0699836775faace4ab5b78b2",
            format: ArchiveFormat::Zip,
        },
        ("windows", "aarch64") => RuntimePackage {
            platform: "windows-arm64",
            version: "1.29.0",
            archive_name: "onnxruntime-win-arm64-1.29.0.zip",
            url: "https://huggingface.co/Duecki/CalibRaw-Artifacts/resolve/91085ce0ec322a4a7cbd20059688690218e52f9a/onnxruntime/windows-arm64/onnxruntime-win-arm64-1.29.0.zip",
            bytes: 81_679_033,
            sha256: "a094a49c3ced0f9fca554647cc7566ae99d93a63a8ce6bf47975561c2de7608e",
            format: ArchiveFormat::Zip,
        },
        (os, arch) => anyhow::bail!(
            "automatic ONNX Runtime is unavailable for {os}/{arch}; select a compatible runtime manually in Settings"
        ),
    };
    Ok(package)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AutomaticOnnxRuntimeInfo {
    pub platform: &'static str,
    pub version: &'static str,
    pub download_bytes: u64,
}

pub fn automatic_onnx_runtime_info() -> Option<AutomaticOnnxRuntimeInfo> {
    let package = runtime_package().ok()?;
    Some(AutomaticOnnxRuntimeInfo {
        platform: package.platform,
        version: package.version,
        download_bytes: package.bytes,
    })
}

pub fn automatic_onnx_runtime_is_installed() -> bool {
    let Ok(package) = runtime_package() else {
        return false;
    };
    let install_dir = crate::desktop_model_cache_root()
        .join("onnxruntime")
        .join(package.platform);
    matches!(
        load_verified_install(&install_dir, package.sha256),
        Ok(Some(_))
    )
}

fn install_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn runtime_download_options() -> DownloadOptions {
    DownloadOptions {
        connect_timeout: Duration::from_secs(45),
        response_timeout: Duration::from_secs(60),
        body_timeout: Duration::from_secs(30 * 60),
        attempts: 5,
        resume: true,
    }
}

pub fn ensure_automatic_onnx_runtime() -> Result<(PathBuf, String)> {
    let _guard = install_lock();
    let package = runtime_package()?;
    let root = crate::desktop_model_cache_root().join("onnxruntime");
    let install_dir = root.join(package.platform);
    if let Some(runtime) = load_verified_install(&install_dir, package.sha256)? {
        return Ok(runtime);
    }

    fs::create_dir_all(&root)
        .with_context(|| format!("create ONNX Runtime cache {}", root.display()))?;
    let archive_path = root.join(package.archive_name);
    let artifact = ModelArtifact {
        name: "CalibRaw automatic ONNX Runtime",
        url: Some(package.url),
        sha256: package.sha256,
        size: ArtifactSize::Exact(package.bytes),
        progress_total: package.bytes,
    };
    ensure_artifact(
        &archive_path,
        artifact,
        runtime_download_options(),
        |_, _| {},
        || Ok(()),
    )
    .with_context(|| format!("download automatic ONNX Runtime for {}", package.platform))?;

    install_archive(&archive_path, package.format, package.sha256, &install_dir)?;
    load_verified_install(&install_dir, package.sha256)?
        .context("automatic ONNX Runtime install is invalid")
}

fn load_verified_install(
    install_dir: &Path,
    expected_archive_sha256: &str,
) -> Result<Option<(PathBuf, String)>> {
    let manifest = match fs::read_to_string(install_dir.join(INSTALL_MANIFEST)) {
        Ok(manifest) => manifest,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("read automatic ONNX Runtime manifest"),
    };
    let mut lines = manifest.lines();
    let Some(archive_sha256) = lines
        .next()
        .and_then(|line| line.strip_prefix("archive_sha256="))
    else {
        return Ok(None);
    };
    let Some(sha256) = lines.next().and_then(|line| line.strip_prefix("sha256=")) else {
        return Ok(None);
    };
    let Some(relative) = lines.next().and_then(|line| line.strip_prefix("path=")) else {
        return Ok(None);
    };
    if archive_sha256 != expected_archive_sha256
        || lines.next().is_some()
        || sha256.len() != 64
        || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Ok(None);
    }
    let relative = Path::new(relative);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Ok(None);
    }
    let runtime_path = install_dir.join(relative);
    if !runtime_path.is_file() || sha256_file_hex(&runtime_path)? != sha256 {
        return Ok(None);
    }
    Ok(Some((runtime_path, sha256.to_owned())))
}

fn install_archive(
    archive_path: &Path,
    format: ArchiveFormat,
    archive_sha256: &str,
    install_dir: &Path,
) -> Result<()> {
    let temporary = install_dir.with_extension(format!("installing.{}", std::process::id()));
    if temporary.exists() {
        fs::remove_dir_all(&temporary)
            .with_context(|| format!("remove stale runtime install {}", temporary.display()))?;
    }
    fs::create_dir_all(&temporary)
        .with_context(|| format!("create runtime install {}", temporary.display()))?;
    let result = (|| {
        match format {
            ArchiveFormat::TarGz => {
                let file = File::open(archive_path)
                    .with_context(|| format!("open {}", archive_path.display()))?;
                let mut archive = tar::Archive::new(GzDecoder::new(file));
                archive
                    .unpack(&temporary)
                    .with_context(|| format!("extract {}", archive_path.display()))?;
            }
            ArchiveFormat::Zip => {
                let file = File::open(archive_path)
                    .with_context(|| format!("open {}", archive_path.display()))?;
                let mut archive = zip::ZipArchive::new(file)
                    .with_context(|| format!("read {}", archive_path.display()))?;
                archive
                    .extract(&temporary)
                    .with_context(|| format!("extract {}", archive_path.display()))?;
            }
        }
        let runtime_path = find_runtime_library(&temporary)?;
        let sha256 = sha256_file_hex(&runtime_path)?;
        let relative = runtime_path
            .strip_prefix(&temporary)
            .context("automatic runtime escaped its install directory")?;
        let relative = relative
            .to_str()
            .context("automatic runtime path is not valid UTF-8")?;
        anyhow::ensure!(
            !relative.contains(['\n', '\r']),
            "automatic runtime path contains a line break"
        );
        fs::write(
            temporary.join(INSTALL_MANIFEST),
            format!("archive_sha256={archive_sha256}\nsha256={sha256}\npath={relative}\n"),
        )
        .context("write automatic ONNX Runtime manifest")?;
        if install_dir.exists() {
            fs::remove_dir_all(install_dir).with_context(|| {
                format!("replace invalid runtime install {}", install_dir.display())
            })?;
        }
        fs::rename(&temporary, install_dir).with_context(|| {
            format!(
                "publish automatic runtime install {}",
                install_dir.display()
            )
        })
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(&temporary);
    }
    result
}

fn find_runtime_library(root: &Path) -> Result<PathBuf> {
    let mut directories = vec![root.to_path_buf()];
    let mut candidates = Vec::new();
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory)
            .with_context(|| format!("inspect runtime directory {}", directory.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if entry.file_type()?.is_dir() {
                directories.push(path);
                continue;
            }
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            let exact = matches!(
                name.as_str(),
                "onnxruntime.dll" | "libonnxruntime.so" | "libonnxruntime.dylib"
            );
            let versioned = name.starts_with("libonnxruntime.so.")
                || (name.starts_with("libonnxruntime.") && name.ends_with(".dylib"));
            if path.is_file() && (exact || versioned) {
                candidates.push((!exact, path));
            }
        }
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    candidates
        .into_iter()
        .next()
        .map(|(_, path)| path)
        .context("downloaded archive contains no ONNX Runtime shared library")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_package_is_pinned_to_calibraw_artifacts() {
        let package = runtime_package().unwrap();
        assert!(package
            .url
            .starts_with("https://huggingface.co/Duecki/CalibRaw-Artifacts/resolve/"));
        assert!(package
            .url
            .contains("/91085ce0ec322a4a7cbd20059688690218e52f9a/onnxruntime/"));
        assert_eq!(package.sha256.len(), 64);
        assert!(package.bytes > 1_000_000);
    }

    #[test]
    fn automatic_runtime_download_retries_and_resumes() {
        let options = runtime_download_options();
        assert!(options.attempts > 1);
        assert!(options.resume);
    }

    #[test]
    #[ignore = "requires a network connection and writes the desktop runtime cache"]
    fn automatic_runtime_downloads_extracts_and_initializes() {
        crate::ai_masks::initialize_runtime(None, None).unwrap();
    }
}
