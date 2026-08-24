use super::super::{ExportDestination, ExportTask, ExportTaskKind, ExportTaskReceiver};
use super::{
    batch::batch_export_overall_fraction,
    export::{clear_export_task, export_source_stem},
};
use crate::pipeline::ExportEvent;
use std::path::Path;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc, Arc,
};

#[test]
fn display_label_overrides_the_materialized_filename_for_exports() {
    assert_eq!(
        export_source_stem(
            Some(Path::new("/cache/asset/original.dng")),
            Some("IMG_1234.DNG")
        ),
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

fn test_export_task() -> ExportTask {
    let (_sender, receiver) = mpsc::channel::<ExportEvent>();
    ExportTask {
        kind: ExportTaskKind::Single,
        cancellation: Arc::new(AtomicBool::new(false)),
        receiver: Some(ExportTaskReceiver::Tiled(receiver)),
        destination: Some(ExportDestination::File("photo.png".into())),
        progress: 0.42,
        phase: "Rendering tile 4/10".to_owned(),
        completed: 0,
        total: 1,
        completed_tiles: 4,
        total_tiles: 10,
        minimized: false,
        cancelling: false,
    }
}

#[test]
fn export_destination_keeps_the_render_path_explicit() {
    let destination = ExportDestination::File("nested/photo.tif".into());
    assert_eq!(destination.path(), Path::new("nested/photo.tif"));
}

#[cfg(target_os = "android")]
#[test]
fn android_gallery_destination_keeps_publish_name_and_format() {
    let destination = ExportDestination::AndroidGallery {
        path: "cache/photo-123.jpg".into(),
        display_name: "photo-auraw.jpg".to_owned(),
        format: crate::pipeline::ExportFormat::Jpeg,
    };
    assert_eq!(destination.path(), Path::new("cache/photo-123.jpg"));
    match destination {
        ExportDestination::AndroidGallery {
            display_name,
            format,
            ..
        } => {
            assert_eq!(display_name, "photo-auraw.jpg");
            assert_eq!(format, crate::pipeline::ExportFormat::Jpeg);
        }
        _ => unreachable!(),
    }
}

#[test]
fn minimized_export_keeps_the_background_worker_active() {
    let mut task = test_export_task();
    task.minimize();
    assert!(task.minimized);
    assert!(task.receiver.is_some());
    assert_eq!(task.progress, 0.42);
    assert!(!task.cancellation.load(Ordering::Acquire));
}

#[test]
fn minimized_export_can_be_restored() {
    let mut task = test_export_task();
    task.minimize();
    task.restore();
    assert!(!task.minimized);
    assert!(task.receiver.is_some());
}

#[test]
fn export_cancellation_sets_the_shared_token_and_state() {
    let mut task = test_export_task();
    task.request_cancel();
    assert!(task.cancelling);
    assert!(task.cancellation.load(Ordering::Acquire));
    assert_eq!(task.phase, "Cancelling export…");
}

#[test]
fn export_completion_clears_the_active_task() {
    let mut slot = Some(test_export_task());
    clear_export_task(&mut slot);
    assert!(slot.is_none());
}

#[test]
fn export_failure_cleanup_clears_the_active_task() {
    let mut slot = Some(test_export_task());
    slot.as_mut().unwrap().phase = "Export failed".to_owned();
    clear_export_task(&mut slot);
    assert!(slot.is_none());
}
