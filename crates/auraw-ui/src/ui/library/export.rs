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
    crate::ui::sidebar::export_settings_controls(ui, settings, None, false, picker_directory);
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
pub(super) fn library_export_jobs(paths: &[PathBuf], format: ExportFormat) -> Option<Vec<(PathBuf, PathBuf)>> {
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
        let mut dialog = rfd::FileDialog::new().set_file_name(default_name);
        if let Some(parent) = source
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            dialog = dialog.set_directory(parent);
        }
        dialog = match format {
            ExportFormat::Png => dialog.add_filter("PNG image", &["png"]),
            ExportFormat::Jpeg => dialog.add_filter("JPEG image", &["jpg", "jpeg"]),
            ExportFormat::Tiff => dialog.add_filter("TIFF image", &["tif", "tiff"]),
        };
        let mut destination = dialog.save_file()?;
        let valid_extension = destination
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| match format {
                ExportFormat::Png => extension.eq_ignore_ascii_case("png"),
                ExportFormat::Jpeg => {
                    extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg")
                }
                ExportFormat::Tiff => {
                    extension.eq_ignore_ascii_case("tif") || extension.eq_ignore_ascii_case("tiff")
                }
            });
        if !valid_extension {
            destination.set_extension(format.extension());
        }
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

pub(super) fn show_library_batch_export_progress(ui: &mut Ui, app: &mut AurawApp) {
    let mut cancel = false;
    #[cfg(not(target_os = "android"))]
    let mut minimize = false;

    if let Some((completed, total, failed, current_name, cancelling)) =
        app.library_batch_export_status()
    {
        if app.library_batch_export_progress_open() {
            let exported = completed.saturating_sub(failed);
            let overall_fraction = app.library_batch_export_overall_fraction().unwrap_or(0.0);

            crate::ui::responsive_popup(egui::Window::new("Exporting images"), ui.ctx(), 420.0)
                .id(egui::Id::new("library-batch-export-progress-dialog"))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ui.ctx(), |ui| {
                    ui.label(
                        egui::RichText::new(format!("{exported} / {total} exported")).strong(),
                    );
                    ui.add_space(6.0);
                    ui.add(
                        egui::ProgressBar::new(overall_fraction)
                            .show_percentage()
                            .animate(!cancelling),
                    );
                    ui.add_space(6.0);

                    if let Some(name) = current_name.as_deref() {
                        let phase = match app.library_batch_export_tile_progress() {
                            Some((tiles_done, tiles_total))
                                if tiles_total > 0 && tiles_done >= tiles_total =>
                            {
                                "Finalizing"
                            }
                            Some((_, tiles_total)) if tiles_total > 0 => "Exporting",
                            _ => "Preparing",
                        };
                        ui.label(format!("{phase} {name}…"));
                    }
                    if failed > 0 {
                        ui.label(
                            egui::RichText::new(format!(
                                "{failed} {} failed",
                                if failed == 1 { "image" } else { "images" }
                            ))
                            .small()
                            .color(ui.visuals().warn_fg_color),
                        );
                    }
                    if cancelling {
                        ui.label(
                            egui::RichText::new("Cancelling after the current image finishes…")
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        #[cfg(not(target_os = "android"))]
                        if ui.button("Minimize").clicked() {
                            minimize = true;
                        }
                        if ui
                            .add_enabled(!cancelling, egui::Button::new("Cancel"))
                            .clicked()
                        {
                            cancel = true;
                        }
                    });
                });
        }
    }

    #[cfg(not(target_os = "android"))]
    if minimize {
        app.minimize_library_batch_export_progress();
    }
    if cancel {
        app.cancel_library_batch_export();
    }
}

