use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use toml_edit::{DocumentMut, Item, Table, Value};

#[derive(Debug)]
pub struct WorkspaceMetadata {
    pub manifest_path: PathBuf,
    pub android_ndk_version: String,
    pub android_build_tools_version: String,
    pub android_compile_sdk: u32,
    pub android_min_sdk: u32,
    pub android_target_sdk: u32,
    pub libraw_revision: String,
    pub lensfun_revision: String,
    pub android_use_legacy_packaging: bool,
}

impl WorkspaceMetadata {
    pub fn load_from_manifest_dir() -> Result<Self, String> {
        let manifest_dir = PathBuf::from(
            env::var_os("CARGO_MANIFEST_DIR")
                .ok_or_else(|| "Cargo did not set CARGO_MANIFEST_DIR".to_owned())?,
        );
        let manifest_path = find_workspace_manifest(&manifest_dir)?;
        let source = fs::read_to_string(&manifest_path)
            .map_err(|error| format!("cannot read {}: {error}", manifest_path.display()))?;
        let values = parse_workspace_metadata(&manifest_path, &source)?;

        let metadata = Self {
            manifest_path,
            android_ndk_version: required_string(&values, "android_ndk_version")?,
            android_build_tools_version: required_string(&values, "android_build_tools_version")?,
            android_compile_sdk: required_u32(&values, "android_compile_sdk")?,
            android_min_sdk: required_u32(&values, "android_min_sdk")?,
            android_target_sdk: required_u32(&values, "android_target_sdk")?,
            libraw_revision: required_string(&values, "libraw_revision")?,
            lensfun_revision: required_string(&values, "lensfun_revision")?,
            android_use_legacy_packaging: required_bool(&values, "android_use_legacy_packaging")?,
        };
        if metadata.android_min_sdk > metadata.android_target_sdk {
            return Err(
                "workspace.metadata.android_min_sdk cannot exceed android_target_sdk".to_owned(),
            );
        }
        if metadata.android_target_sdk > metadata.android_compile_sdk {
            return Err(
                "workspace.metadata.android_target_sdk cannot exceed android_compile_sdk"
                    .to_owned(),
            );
        }
        Ok(metadata)
    }

    pub fn emit_cargo_contract(&self) {
        println!("cargo:rerun-if-changed={}", self.manifest_path.display());
        for (name, value) in [
            (
                "AURAW_ANDROID_NDK_VERSION",
                self.android_ndk_version.as_str(),
            ),
            (
                "AURAW_ANDROID_BUILD_TOOLS_VERSION",
                self.android_build_tools_version.as_str(),
            ),
            ("AURAW_LIBRAW_REVISION", self.libraw_revision.as_str()),
            ("AURAW_LENSFUN_REVISION", self.lensfun_revision.as_str()),
        ] {
            println!("cargo:rustc-env={name}={value}");
        }
        println!(
            "cargo:rustc-env=AURAW_ANDROID_COMPILE_SDK={}",
            self.android_compile_sdk
        );
        println!(
            "cargo:rustc-env=AURAW_ANDROID_MIN_SDK={}",
            self.android_min_sdk
        );
        println!(
            "cargo:rustc-env=AURAW_ANDROID_TARGET_SDK={}",
            self.android_target_sdk
        );
        println!(
            "cargo:rustc-env=AURAW_ANDROID_USE_LEGACY_PACKAGING={}",
            self.android_use_legacy_packaging
        );
    }
}

fn find_workspace_manifest(manifest_dir: &Path) -> Result<PathBuf, String> {
    for ancestor in manifest_dir.ancestors() {
        let candidate = ancestor.join("Cargo.toml");
        let Ok(source) = fs::read_to_string(&candidate) else {
            continue;
        };
        if source.lines().any(|line| line.trim() == "[workspace]") {
            return Ok(candidate);
        }
    }
    Err(format!(
        "cannot find a workspace Cargo.toml above CARGO_MANIFEST_DIR={}",
        manifest_dir.display()
    ))
}

fn parse_workspace_metadata(path: &Path, source: &str) -> Result<Table, String> {
    let document = source
        .parse::<DocumentMut>()
        .map_err(|error| format!("cannot parse {} as TOML: {error}", path.display()))?;
    let metadata = document
        .get("workspace")
        .and_then(Item::as_table)
        .and_then(|workspace| workspace.get("metadata"))
        .and_then(Item::as_table)
        .ok_or_else(|| {
            format!(
                "{} is missing a populated [workspace.metadata] table",
                path.display()
            )
        })?;
    if metadata.is_empty() {
        return Err(format!(
            "{} is missing a populated [workspace.metadata] table",
            path.display()
        ));
    }
    Ok(metadata.clone())
}

fn required_string(values: &Table, key: &str) -> Result<String, String> {
    let item = required(values, key)?;
    let value = item
        .as_value()
        .and_then(Value::as_str)
        .ok_or_else(|| format!("workspace.metadata.{key} must be a plain TOML string"))?;
    let encoded = item.to_string();
    let encoded = encoded.trim();
    if encoded.len() < 2 || !encoded.starts_with('"') || !encoded.ends_with('"') {
        return Err(format!(
            "workspace.metadata.{key} must be a plain TOML string"
        ));
    }
    let raw_value = &encoded[1..encoded.len() - 1];
    if value.is_empty() || raw_value.contains('"') || raw_value.contains('\\') {
        return Err(format!(
            "workspace.metadata.{key} must be a non-empty unescaped TOML string"
        ));
    }
    Ok(value.to_owned())
}

fn required_u32(values: &Table, key: &str) -> Result<u32, String> {
    let value = required(values, key)?
        .as_value()
        .and_then(Value::as_integer)
        .and_then(|value| u32::try_from(value).ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| format!("workspace.metadata.{key} must be a positive integer"))?;
    Ok(value)
}

fn required_bool(values: &Table, key: &str) -> Result<bool, String> {
    required(values, key)?
        .as_value()
        .and_then(Value::as_bool)
        .ok_or_else(|| format!("workspace.metadata.{key} must be a boolean"))
}

fn required<'a>(values: &'a Table, key: &str) -> Result<&'a Item, String> {
    values
        .get(key)
        .ok_or_else(|| format!("workspace.metadata.{key} is required"))
}
