use crate::pipeline::CfaKind;

impl Sidebar {
    fn show_info(ui: &mut Ui, app: &CalibRawApp) {
        let Some(raw) = app
            .develop
            .original_raw
            .as_deref()
            .or(app.develop.loaded_raw.as_deref())
        else {
            crate::ui::theme::content_card(ui, |ui| {
                ui.label("Open a RAW image to view its metadata.");
            });
            return;
        };

        let file_name = display_file_name(
            app.develop.current_label.as_deref(),
            app.develop.current_path.as_deref(),
        );

        crate::ui::theme::section_card(ui, "File", |ui| {
            metadata_row(ui, "Name", &file_name);
            #[cfg(not(target_os = "android"))]
            if let Some(path) = app.develop.current_path.as_deref() {
                if let Some(parent) = path.parent() {
                    metadata_row(ui, "Location", &parent.display().to_string());
                }
                if let Some(bytes) = app.library.desktop_asset_bytes(path) {
                    metadata_row(ui, "File size", &format_file_size(bytes));
                }
            }
        });

        crate::ui::theme::card_gap(ui);
        crate::ui::theme::section_card(ui, "Capture", |ui| {
            optional_metadata_row(ui, "ISO", format_iso(raw.capture_metadata.iso_speed));
            optional_metadata_row(
                ui,
                "Shutter speed",
                format_shutter_speed(raw.capture_metadata.shutter_seconds),
            );
            optional_metadata_row(ui, "Aperture", format_aperture(raw.aperture));
            optional_metadata_row(ui, "Focal length", format_focal_length(raw.focal_length));
            optional_metadata_row(ui, "Focus distance", format_focus_distance(raw.focus_distance));
            optional_metadata_row(ui, "Flash", raw.capture_metadata.flash.map(format_flash));
        });

        crate::ui::theme::card_gap(ui);
        crate::ui::theme::section_card(ui, "Equipment", |ui| {
            optional_metadata_row(ui, "Camera", equipment_name(&raw.camera_make, &raw.camera_model));
            optional_metadata_row(ui, "Lens", equipment_name(&raw.lens_make, &raw.lens_model));
        });

        crate::ui::theme::card_gap(ui);
        crate::ui::theme::section_card(ui, "Image", |ui| {
            metadata_row(ui, "Dimensions", &format!("{} × {} px", raw.width, raw.height));
            let megapixels = f64::from(raw.width) * f64::from(raw.height) / 1_000_000.0;
            metadata_row(ui, "Resolution", &format!("{megapixels:.1} MP"));
            metadata_row(
                ui,
                "Sensor pattern",
                match raw.cfa_kind {
                    CfaKind::Bayer => "Bayer",
                    CfaKind::XTrans => "X-Trans",
                },
            );

            let cropped = app
                .develop
                .geometry
                .crop_pixel_dimensions(raw.width, raw.height);
            if cropped != (raw.width, raw.height) {
                metadata_row(ui, "Cropped size", &format!("{} × {} px", cropped.0, cropped.1));
            }
        });

        if !raw.capture_metadata.artist.trim().is_empty()
            || !raw.capture_metadata.description.trim().is_empty()
        {
            crate::ui::theme::card_gap(ui);
            crate::ui::theme::section_card(ui, "Description", |ui| {
                if !raw.capture_metadata.artist.trim().is_empty() {
                    metadata_row(ui, "Creator", raw.capture_metadata.artist.trim());
                }
                if !raw.capture_metadata.description.trim().is_empty() {
                    metadata_row(ui, "Caption", raw.capture_metadata.description.trim());
                }
            });
        }
    }
}

fn metadata_row(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal_top(|ui| {
        ui.add_sized(
            [92.0, 18.0],
            egui::Label::new(
                egui::RichText::new(label)
                    .small()
                    .color(ui.visuals().weak_text_color()),
            ),
        );
        ui.add(egui::Label::new(value).wrap());
    });
}

fn optional_metadata_row(ui: &mut Ui, label: &str, value: Option<String>) {
    if let Some(value) = value {
        metadata_row(ui, label, &value);
    } else {
        metadata_row(ui, label, "Not recorded");
    }
}

fn finite_positive(value: f32) -> Option<f32> {
    (value.is_finite() && value > 0.0).then_some(value)
}

fn display_file_name(label: Option<&str>, path: Option<&std::path::Path>) -> String {
    label
        .filter(|label| !label.trim().is_empty())
        .and_then(|label| std::path::Path::new(label).file_name())
        .or_else(|| path.and_then(std::path::Path::file_name))
        .and_then(std::ffi::OsStr::to_str)
        .or_else(|| label.filter(|label| !label.trim().is_empty()))
        .unwrap_or("Not available")
        .to_owned()
}

fn format_flash(value: u16) -> String {
    let mut details = Vec::with_capacity(4);
    if value & 0x20 != 0 {
        details.push("No flash function");
    } else if value & 0x01 != 0 {
        details.push("Fired");
    } else {
        details.push("Did not fire");
    }

    match value & 0x18 {
        0x08 => details.push("compulsory mode"),
        0x10 => details.push("suppressed"),
        0x18 => details.push("auto mode"),
        _ => {}
    }
    match value & 0x06 {
        0x02 => details.push("return not detected"),
        0x06 => details.push("return detected"),
        _ => {}
    }
    if value & 0x40 != 0 {
        details.push("red-eye reduction");
    }
    details.join(", ")
}

fn format_iso(value: f32) -> Option<String> {
    finite_positive(value).map(|value| {
        if (value - value.round()).abs() < 0.05 {
            format!("ISO {:.0}", value)
        } else {
            format!("ISO {value:.1}")
        }
    })
}

fn format_shutter_speed(seconds: f32) -> Option<String> {
    finite_positive(seconds).map(|seconds| {
        if seconds < 0.5 {
            let denominator = (1.0 / seconds).round().max(1.0);
            format!("1/{denominator:.0} s")
        } else if (seconds - seconds.round()).abs() < 0.05 {
            format!("{seconds:.0} s")
        } else {
            format!("{seconds:.1} s")
        }
    })
}

fn format_aperture(value: f32) -> Option<String> {
    finite_positive(value).map(|value| format!("f/{value:.1}"))
}

fn format_focal_length(value: f32) -> Option<String> {
    finite_positive(value).map(|value| {
        if (value - value.round()).abs() < 0.05 {
            format!("{value:.0} mm")
        } else {
            format!("{value:.1} mm")
        }
    })
}

fn format_focus_distance(value: f32) -> Option<String> {
    finite_positive(value).map(|value| format!("{value:.2} m"))
}

fn equipment_name(make: &str, model: &str) -> Option<String> {
    let make = make.trim();
    let model = model.trim();
    match (make.is_empty(), model.is_empty()) {
        (true, true) => None,
        (false, true) => Some(make.to_owned()),
        (true, false) => Some(model.to_owned()),
        (false, false) if model.to_ascii_lowercase().starts_with(&make.to_ascii_lowercase()) => {
            Some(model.to_owned())
        }
        (false, false) => Some(format!("{make} {model}")),
    }
}

#[cfg(not(target_os = "android"))]
fn format_file_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

#[cfg(test)]
mod info_tests {
    use super::*;

    #[test]
    fn formats_common_capture_values() {
        assert_eq!(format_iso(400.0).as_deref(), Some("ISO 400"));
        assert_eq!(format_shutter_speed(1.0 / 125.0).as_deref(), Some("1/125 s"));
        assert_eq!(format_shutter_speed(0.8).as_deref(), Some("0.8 s"));
        assert_eq!(format_shutter_speed(2.0).as_deref(), Some("2 s"));
        assert_eq!(format_aperture(2.8).as_deref(), Some("f/2.8"));
        assert_eq!(format_focal_length(50.0).as_deref(), Some("50 mm"));
    }

    #[test]
    fn omits_invalid_capture_values() {
        assert_eq!(format_iso(0.0), None);
        assert_eq!(format_shutter_speed(f32::NAN), None);
        assert_eq!(equipment_name("", ""), None);
    }

    #[test]
    fn equipment_name_does_not_repeat_make() {
        assert_eq!(
            equipment_name("Canon", "Canon EOS R5").as_deref(),
            Some("Canon EOS R5")
        );
        assert_eq!(
            equipment_name("FUJIFILM", "X-T5").as_deref(),
            Some("FUJIFILM X-T5")
        );
    }

    #[test]
    fn file_name_is_reduced_to_its_basename() {
        assert_eq!(
            display_file_name(
                Some("/run/media/photos/RAW08061 copy.ARW"),
                Some(std::path::Path::new("/fallback/other.ARW")),
            ),
            "RAW08061 copy.ARW"
        );
    }

    #[test]
    fn formats_exif_flash_bits() {
        assert_eq!(format_flash(0), "Did not fire");
        assert_eq!(format_flash(0x10), "Did not fire, suppressed");
        assert_eq!(
            format_flash(0x5f),
            "Fired, auto mode, return detected, red-eye reduction"
        );
    }
}
