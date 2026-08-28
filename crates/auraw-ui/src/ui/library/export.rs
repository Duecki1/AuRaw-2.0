use super::*;

fn enforce_library_export_bit_depth(format: ExportFormat, settings: &mut ExportSettings) {
    match format {
        ExportFormat::Jpeg => settings.bit_depth = crate::pipeline::ExportBitDepth::Eight,
        ExportFormat::Png => {
            if settings.bit_depth == crate::pipeline::ExportBitDepth::Float32Linear {
                settings.bit_depth = crate::pipeline::ExportBitDepth::Sixteen;
            }
        }
        _ => {}
    }
}

pub(super) fn show_library_export_settings_controls(
    ui: &mut Ui,
    format: &mut ExportFormat,
    settings: &mut ExportSettings,
    picker_directory: Option<&Path>,
) {
    ui.horizontal(|ui| {
        ui.label("Format");
        ui.selectable_value(format, ExportFormat::Jpeg, "JPEG");
        ui.selectable_value(format, ExportFormat::Png, "PNG");
        ui.selectable_value(format, ExportFormat::Tiff, "TIFF");
    });
    enforce_library_export_bit_depth(*format, settings);
    ui.add_space(6.0);
    crate::ui::sidebar::export_settings_controls(ui, settings, picker_directory);
    enforce_library_export_bit_depth(*format, settings);
}

#[cfg(not(target_os = "android"))]
pub(super) fn unique_library_export_path(
    folder: &Path,
    source: &Path,
    format: ExportFormat,
    reserved: &mut HashSet<PathBuf>,
) -> PathBuf {
    let stem = source
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|stem| !stem.is_empty())
        .unwrap_or("auraw-export");
    let base = format!("{stem}-auraw");
    let mut index = 1usize;
    loop {
        let name = if index == 1 {
            format!("{base}.{}", format.extension())
        } else {
            format!("{base}-{index}.{}", format.extension())
        };
        let candidate = folder.join(name);
        if !candidate.exists() && reserved.insert(candidate.clone()) {
            return candidate;
        }
        index += 1;
    }
}

#[cfg(not(target_os = "android"))]
pub(super) fn library_export_jobs(
    paths: &[PathBuf],
    format: ExportFormat,
) -> Option<Vec<(PathBuf, PathBuf)>> {
    if paths.is_empty() {
        return None;
    }
    if paths.len() == 1 {
        let source = &paths[0];
        let default_name = source
            .file_stem()
            .and_then(|name| name.to_str())
            .map(|name| format!("{name}-auraw.{}", format.extension()))
            .unwrap_or_else(|| format!("auraw-export.{}", format.extension()));
        let destination =
            crate::ui::choose_export_file_path(format, &default_name, source.parent())?;
        return Some(vec![(source.clone(), destination)]);
    }

    let mut dialog = rfd::FileDialog::new();
    if let Some(parent) = paths
        .first()
        .and_then(|path| path.parent())
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        dialog = dialog.set_directory(parent);
    }
    let folder = dialog.pick_folder()?;
    let mut reserved = HashSet::new();
    Some(
        paths
            .iter()
            .map(|source| {
                let destination =
                    unique_library_export_path(&folder, source, format, &mut reserved);
                (source.clone(), destination)
            })
            .collect(),
    )
}
