use super::*;

impl AurawApp {
    pub(in crate::app) fn ai_model_root(&self) -> PathBuf {
        #[cfg(not(target_os = "android"))]
        {
            auraw_ai::desktop_model_cache_root()
        }
        #[cfg(target_os = "android")]
        {
            self.android.android_app
                .internal_data_path()
                .unwrap_or_else(std::env::temp_dir)
                .join("models")
        }
    }

    pub(in crate::app) fn sam21_model_paths(&self) -> (PathBuf, PathBuf) {
        let root = self.ai_model_root();
        (
            root.join("sam2.1-hiera-tiny.encoder.onnx"),
            root.join("sam2.1-hiera-tiny.decoder.onnx"),
        )
    }

    pub(in crate::app) fn birefnet_model_path(&self) -> PathBuf {
        self.ai_model_root()
            .join(self.ai.birefnet_quality.model().cache_filename)
    }

    pub(in crate::app) fn vitmatte_model_path(&self) -> PathBuf {
        self.ai_model_root().join("vitmatte-small-composition-1k.onnx")
    }

    pub(in crate::app) fn landscape_model_path(&self) -> PathBuf {
        self.ai_model_root()
            .join("maskformer-swin-base-ade20k-int8.onnx")
    }

    pub(in crate::app) fn big_lama_model_path(&self) -> PathBuf {
        self.ai_model_root().join(crate::remove::BIG_LAMA_MODEL_FILENAME)
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn onnx_runtime_config_path() -> PathBuf {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(std::env::temp_dir);
        root.join("auraw/onnx-runtime-path")
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn load_onnx_runtime_selection() -> Option<(PathBuf, String)> {
        let configured = std::fs::read_to_string(Self::onnx_runtime_config_path()).ok()?;
        let mut lines = configured.lines();
        let sha256 = lines.next()?.strip_prefix("sha256=")?.to_owned();
        let path = PathBuf::from(lines.next()?.strip_prefix("path=")?);
        if lines.next().is_some()
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || !path.is_file()
        {
            return None;
        }
        Some((path, sha256))
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn persist_onnx_runtime_selection(
        selection: Option<(&std::path::Path, &str)>,
    ) -> Result<(), String> {
        let config = Self::onnx_runtime_config_path();
        if let Some((path, sha256)) = selection {
            let parent = config
                .parent()
                .ok_or_else(|| "invalid AuRaw configuration path".to_owned())?;
            let path_text = path
                .to_str()
                .ok_or_else(|| "the ONNX Runtime path is not valid UTF-8".to_owned())?;
            if path_text.contains('\n') || path_text.contains('\r') {
                return Err("the ONNX Runtime path contains a line break".to_owned());
            }
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
            let temporary = config.with_extension(format!("tmp.{}", std::process::id()));
            let payload = format!("sha256={sha256}\npath={path_text}\n");
            std::fs::write(&temporary, payload.as_bytes())
                .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
            #[cfg(windows)]
            if config.exists() {
                std::fs::remove_file(&config)
                    .map_err(|error| format!("could not replace {}: {error}", config.display()))?;
            }
            std::fs::rename(&temporary, &config)
                .map_err(|error| format!("could not publish {}: {error}", config.display()))?;
        } else if let Err(error) = std::fs::remove_file(&config) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("could not remove {}: {error}", config.display()));
            }
        }
        Ok(())
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn choose_onnx_runtime(&mut self) {
        if self.ui.desktop_picker_receiver.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new()
            .set_title("Select the ONNX Runtime shared library");
        if let Some(parent) = self.ai.runtime_path
            .as_deref()
            .and_then(|path| path.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            dialog = dialog.set_directory(parent);
        }
        self.ui.desktop_picker_receiver = Some(spawn_ui_worker(&self.egui_ctx, move || {
            let result = pollster::block_on(dialog.pick_file())
                .map(|handle| handle.path().to_path_buf())
                .map(Self::validate_and_persist_onnx_runtime)
                .transpose();
            crate::app::DesktopPickerEvent::OnnxRuntime(result)
        }));
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn validate_and_persist_onnx_runtime(path: PathBuf) -> Result<(PathBuf, String), String> {
        if !path.is_file() {
            return Err(format!("{} is not a file.", path.display()));
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let looks_like_runtime = if cfg!(target_os = "windows") {
            file_name == "onnxruntime.dll"
        } else if cfg!(target_os = "macos") {
            file_name == "libonnxruntime.dylib"
                || (file_name.starts_with("libonnxruntime.") && file_name.ends_with(".dylib"))
        } else {
            file_name == "libonnxruntime.so" || file_name.starts_with("libonnxruntime.so.")
        };
        if !looks_like_runtime {
            return Err(
                "Select the ONNX Runtime shared library (onnxruntime.dll, libonnxruntime.so, or libonnxruntime.dylib)."
                    .to_owned(),
            );
        }
        let sha256 = crate::ai_masks::sha256_file_hex(&path)
            .map_err(|error| format!("Could not hash selected ONNX Runtime: {error:#}"))?;
        if let Err(error) = crate::ai_masks::probe_runtime_subprocess(&path, &sha256) {
            return Err(format!(
                "This ONNX Runtime could not be loaded safely: {error:#}"
            ));
        }
        Self::persist_onnx_runtime_selection(Some((&path, &sha256)))?;
        Ok((path, sha256))
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn clear_onnx_runtime(&mut self) {
        match Self::persist_onnx_runtime_selection(None) {
            Ok(()) => {
                self.ai.runtime_path = None;
                self.ai.runtime_sha256 = None;
                self.ui.notice = Some(
                    "ONNX Runtime selection cleared. Restart AuRaw to apply the change.".to_owned(),
                );
            }
            Err(error) => self.ui.notice = Some(error),
        }
    }
}
