use super::{batch::batch_export_overall_fraction, export::export_source_stem};
use std::path::Path;

#[test]
fn display_label_overrides_the_materialized_filename_for_exports() {
    assert_eq!(
        export_source_stem(Some(Path::new("/cache/asset/original.dng")), Some("IMG_1234.DNG")),
        "IMG_1234"
    );
}

#[test]
fn completed_images_do_not_reach_full_progress_early() {
    let progress = batch_export_overall_fraction(2, 3, false, None);
    assert!((progress - (2.0 / 3.0)).abs() < f32::EPSILON);
    assert!(progress < 1.0);
}

#[test]
fn fully_rendered_current_image_reserves_finalization_progress() {
    let progress = batch_export_overall_fraction(2, 3, true, Some((10, 10)));
    assert!((progress - (2.9 / 3.0)).abs() < 0.000_01);
    assert!(progress < 1.0);
}

#[test]
fn batch_reaches_one_only_after_every_image_is_finished() {
    assert_eq!(batch_export_overall_fraction(3, 3, false, None), 1.0);
}

#[test]
fn stale_tile_progress_is_ignored_without_a_current_image() {
    let progress = batch_export_overall_fraction(1, 3, false, Some((10, 10)));
    assert!((progress - (1.0 / 3.0)).abs() < f32::EPSILON);
}
