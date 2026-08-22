use crate::model_artifact::{
    ensure_artifact, verify_artifact, DownloadOptions, ModelArtifact,
};
use anyhow::Result;
use std::path::Path;

#[derive(Clone, Copy, Debug)]
pub(crate) struct ModelInstallSpec {
    pub artifact: ModelArtifact,
    pub download: DownloadOptions,
    pub progress_label: &'static str,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModelDownloadProgress {
    pub label: &'static str,
    pub downloaded: u64,
    pub total: u64,
}

impl ModelInstallSpec {
    pub(crate) fn is_installed(self, path: &Path) -> bool {
        verify_artifact(path, self.artifact).is_ok()
    }

    pub(crate) fn ensure_installed<F, C>(
        self,
        path: &Path,
        allow_download: bool,
        mut progress: F,
        cancellation: C,
    ) -> Result<()>
    where
        F: FnMut(ModelDownloadProgress),
        C: FnMut() -> Result<()>,
    {
        if !allow_download {
            return verify_artifact(path, self.artifact).map_err(|error| {
                anyhow::anyhow!(
                    "the pinned {} is unavailable or invalid ({error:#}); consent to its download again",
                    self.artifact.name
                )
            });
        }

        ensure_artifact(
            path,
            self.artifact,
            self.download,
            |downloaded, total| {
                progress(ModelDownloadProgress {
                    label: self.progress_label,
                    downloaded,
                    total,
                });
            },
            cancellation,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model_artifact::ArtifactSize;
    use std::fs;

    const TEST_ARTIFACT: ModelArtifact = ModelArtifact {
        name: "test installed model",
        url: None,
        sha256: "7702832f291b1ad6d8269d712184a9ddc87c9bac3833fa10b3f2140830fb4c47",
        size: ArtifactSize::Exact(17),
        progress_total: 17,
    };
    const TEST_INSTALL: ModelInstallSpec = ModelInstallSpec {
        artifact: TEST_ARTIFACT,
        download: DownloadOptions {
            connect_timeout: std::time::Duration::from_secs(1),
            response_timeout: std::time::Duration::from_secs(1),
            body_timeout: std::time::Duration::from_secs(1),
            attempts: 1,
            resume: false,
        },
        progress_label: "test model",
    };

    #[test]
    fn verified_model_is_not_downloaded_again() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model.onnx");
        fs::write(&path, b"valid model bytes").unwrap();
        let mut progress_calls = 0;
        TEST_INSTALL
            .ensure_installed(
                &path,
                true,
                |_| progress_calls += 1,
                || Ok(()),
            )
            .unwrap();
        assert_eq!(progress_calls, 0);
    }

    #[test]
    fn missing_model_requires_download_consent() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("model.onnx");
        let error = TEST_INSTALL
            .ensure_installed(&path, false, |_| {}, || Ok(()))
            .unwrap_err();
        assert!(format!("{error:#}").contains("consent to its download again"));
    }
}
