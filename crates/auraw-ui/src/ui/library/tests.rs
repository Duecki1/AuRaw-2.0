#[cfg(not(target_os = "android"))]
use super::LibrarySource;
use super::{
    android_folder_ancestors, android_folder_parent, android_library_location_label,
    balanced_justified_row_ranges, catalog_status, cloud_cache_icon,
    cloud_folder_id_for_catalog, cloud_preview_icon, cloud_preview_label, cloud_preview_notice,
    cloud_sync_badge, copy_directory_create_new, duplicate_raw_and_sidecar, elide_middle,
    format_file_size, import_folder_into_library, import_raw_into_folder,
    justified_thumbnail_layout, library_import_fab_rect, library_import_icon,
    loaded_library_thumbnail, make_resident_thumbnail, new_library_entry, rename_raw_bundle,
    run_folder_operation, run_image_paste, run_thumbnail_workers, scan_folder,
    scan_folder_tree, scan_folder_with_limit, trash_age_label, trash_remaining_label,
    trash_size_label, validate_folder_name, ImageClipboard, ImageClipboardContent,
    ImageClipboardMode, ImagePasteDestination, LibraryFileInfo, LibraryFolderOperation,
    LibraryState, LibraryThumbnailSize, LibraryView, RawImportOutcome, ScanEvent,
    ThumbnailRequest, ThumbnailWorker,
};
use crate::pipeline::RawThumbnail;
use eframe::egui::Color32;
use std::collections::HashSet;
use std::fs;
#[cfg(not(target_os = "android"))]
use std::io::{Read, Write};
#[cfg(not(target_os = "android"))]
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

#[test]
fn android_folder_navigation_uses_normalized_relative_paths() {
    assert_eq!(android_folder_parent("2026/Trip"), "2026");
    assert_eq!(android_folder_parent("2026"), "");
    assert_eq!(
        android_folder_ancestors("2026/Trip"),
        HashSet::from([String::new(), "2026".to_owned()])
    );
    assert_eq!(
        android_library_location_label("/media/.library", "2026/Trip"),
        "/media/.library/2026/Trip"
    );
}

#[test]
fn successful_catalog_status_does_not_repeat_the_header_file_count() {
    assert_eq!(catalog_status(0, false), "");
    assert_eq!(catalog_status(1, false), "1 unreadable item");
    assert!(catalog_status(0, true).contains("RAW files shown"));
}

#[cfg(not(target_os = "android"))]
fn read_http_request(stream: &mut std::net::TcpStream) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "client closed before sending HTTP headers");
        request.extend_from_slice(&buffer[..count]);
        if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let content_length = String::from_utf8_lossy(&request[..header_end])
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().unwrap())
        })
        .unwrap_or(0);
    while request.len() < header_end + content_length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "client closed before sending HTTP body");
        request.extend_from_slice(&buffer[..count]);
    }
    request
}

#[cfg(not(target_os = "android"))]
fn write_http_response(stream: &mut std::net::TcpStream, content_type: &str, body: &[u8]) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .unwrap();
    stream.write_all(body).unwrap();
}

#[cfg(not(target_os = "android"))]
fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(ring::digest::digest(&ring::digest::SHA256, bytes).as_ref())
}

#[cfg(not(target_os = "android"))]
fn test_developed_thumbnail() -> RawThumbnail {
    RawThumbnail {
        width: 16,
        height: 12,
        rgba: [28, 74, 196, 255].repeat(16 * 12),
    }
}

#[cfg(not(target_os = "android"))]
fn install_test_developed_thumbnail(raw: &std::path::Path) {
    let fingerprint = crate::sidecar::desktop_sidecar_fingerprint(raw)
        .unwrap()
        .expect("test RAW should have an edit sidecar");
    crate::sidecar::save_developed_thumbnail_cache(
        raw,
        &test_developed_thumbnail(),
        fingerprint,
    )
    .unwrap();
}

#[cfg(not(target_os = "android"))]
fn assert_test_developed_thumbnail(raw: &std::path::Path) {
    let thumbnail = crate::sidecar::load_developed_thumbnail_cache(raw, 512)
        .unwrap()
        .expect("copied RAW should retain its developed thumbnail");
    assert_eq!([thumbnail.width, thumbnail.height], [16, 12]);
    let pixel = &thumbnail.rgba[..4];
    assert!(pixel[2] > pixel[1] && pixel[1] > pixel[0], "{pixel:?}");
}

#[cfg(not(target_os = "android"))]
fn test_developed_thumbnail_jpeg() -> Vec<u8> {
    let thumbnail = test_developed_thumbnail();
    let rgba =
        image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.rgba).unwrap();
    let rgb = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 92)
        .encode(
            rgb.as_raw(),
            thumbnail.width,
            thumbnail.height,
            image::ExtendedColorType::Rgb8,
        )
        .unwrap();
    jpeg
}

#[test]
fn missing_cloud_catalog_folder_falls_back_to_root() {
    let folder = crate::cloud::CloudFolder {
        id: "a".repeat(64),
        parent_id: crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
        name: "Trips".to_owned(),
    };
    assert_eq!(
        cloud_folder_id_for_catalog(&folder.id, std::slice::from_ref(&folder)),
        folder.id.clone()
    );
    assert_eq!(
        cloud_folder_id_for_catalog(&"b".repeat(64), &[folder]),
        crate::cloud::CLOUD_ROOT_FOLDER_ID
    );
}

#[test]
fn middle_elision_preserves_both_ends() {
    assert_eq!(elide_middle("abcdefghijklmnop", 9), "abcd…mnop");
    assert_eq!(elide_middle("short", 9), "short");
}

#[test]
fn file_sizes_are_readable() {
    assert_eq!(format_file_size(500), "500 B");
    assert_eq!(format_file_size(1024), "1.0 KiB");
    assert_eq!(format_file_size(2 * 1024 * 1024), "2.0 MiB");
}

#[test]
fn cloud_trash_retention_metadata_is_readable() {
    assert_eq!(trash_age_label(0), "just now");
    assert_eq!(trash_age_label(2 * 24 * 60 * 60), "2 days ago");
    assert_eq!(trash_remaining_label(25 * 60 * 60), "2 days remaining");
    assert_eq!(trash_size_label(2 * 1024 * 1024), "2.0 MiB");
}

#[test]
fn thumbnail_size_defaults_to_average_with_small_preserving_the_old_scale() {
    assert_eq!(
        LibraryThumbnailSize::default(),
        LibraryThumbnailSize::Medium
    );
    assert_eq!(LibraryThumbnailSize::Small.scale(), 1.0);
    assert_eq!(LibraryThumbnailSize::Medium.scale(), 1.25);
    assert_eq!(LibraryThumbnailSize::Large.scale(), 1.5);
    assert_eq!(LibraryThumbnailSize::Enormous.scale(), 1.75);
}

#[cfg(not(target_os = "android"))]
#[test]
fn thumbnail_checkbox_enters_toggles_and_exits_selection() {
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);
    let source = LibrarySource::File(PathBuf::from("selection.dng"));

    assert!(library.toggle_thumbnail_selection(&source));
    assert!(library.selection_mode());
    assert!(library.selected_sources.contains(&source));

    assert!(!library.toggle_thumbnail_selection(&source));
    assert!(!library.selection_mode());
    assert!(library.selected_sources.is_empty());
}

#[cfg(not(target_os = "android"))]
#[test]
fn multi_selection_rejects_single_image_actions() {
    let paths = vec![PathBuf::from("first.dng"), PathBuf::from("second.dng")];

    assert!(super::desktop_selection_action(
        super::SelectionBarCommand::CopyAdjustments,
        &paths,
    )
    .is_none());
    assert!(
        super::desktop_selection_action(super::SelectionBarCommand::Rename, &paths).is_none()
    );
    assert!(
        super::desktop_selection_action(super::SelectionBarCommand::Export, &paths).is_some()
    );
}

#[cfg(not(target_os = "android"))]
#[test]
fn cloud_sources_support_multi_selection_and_nested_breadcrumbs() {
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);
    let parent = crate::cloud::CloudFolder {
        id: "a".repeat(64),
        parent_id: crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
        name: "Trips".to_owned(),
    };
    let child = crate::cloud::CloudFolder {
        id: "b".repeat(64),
        parent_id: parent.id.clone(),
        name: "Day 1".to_owned(),
    };
    library.cloud_folders = vec![parent.clone(), child.clone()];
    library.cloud_folder_id = child.id.clone();
    assert_eq!(
        library.cloud_folder_path(&child.id),
        "Cloud / Trips / Day 1"
    );
    assert_eq!(
        library.cloud_breadcrumbs(),
        vec![
            (
                crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
                "Cloud".to_owned()
            ),
            (parent.id, parent.name),
            (child.id, child.name),
        ]
    );

    let asset = crate::cloud::CloudAsset {
        id: "c".repeat(64),
        name: "photo.dng".to_owned(),
        bytes: 10,
        modified_seconds: 1,
        width: 10,
        height: 10,
        raw_etag: "d".repeat(64),
        sidecar_etag: None,
        thumbnail_etag: "e".repeat(64),
        thumbnail_kind: crate::cloud::CloudThumbnailKind::Edited,
        folder_id: "b".repeat(64),
    };
    let source = LibrarySource::Cloud(asset);
    assert!(library.toggle_thumbnail_selection(&source));
    assert!(library.selected_sources.contains(&source));
}

#[test]
fn import_fab_is_square_bottom_right_and_uses_plus_icon() {
    let bounds = eframe::egui::Rect::from_min_size(
        eframe::egui::pos2(10.0, 20.0),
        eframe::egui::vec2(300.0, 400.0),
    );
    let rect = library_import_fab_rect(bounds);
    assert_eq!(
        rect.size(),
        eframe::egui::Vec2::splat(crate::ui::theme::FLOATING_ACTION_EDGE)
    );
    assert_eq!(
        rect.right_bottom(),
        bounds.right_bottom()
            - eframe::egui::Vec2::splat(crate::ui::theme::FLOATING_ACTION_MARGIN)
    );
    assert_eq!(library_import_icon(), egui_phosphor::regular::PLUS);
}

#[test]
fn cloud_cache_icons_distinguish_remote_and_downloaded_raws() {
    assert_eq!(cloud_cache_icon(false), egui_phosphor::regular::CLOUD);
    assert_eq!(cloud_cache_icon(true), egui_phosphor::regular::DOWNLOAD);
}

#[test]
fn cloud_sync_badges_distinguish_queue_failure_and_conflict() {
    let (queued_icon, queued_color, _) =
        cloud_sync_badge(crate::cloud::CloudSyncState::Queued, true);
    assert_eq!(queued_icon, egui_phosphor::regular::ARROW_CLOCKWISE);
    assert_eq!(queued_color, Color32::from_rgb(245, 190, 55));

    let (failed_icon, failed_color, _) =
        cloud_sync_badge(crate::cloud::CloudSyncState::Failed, true);
    assert_eq!(failed_icon, egui_phosphor::regular::X);
    assert_eq!(failed_color, Color32::from_rgb(240, 78, 78));

    let (conflict_icon, conflict_color, _) =
        cloud_sync_badge(crate::cloud::CloudSyncState::Conflict, true);
    assert_eq!(conflict_icon, egui_phosphor::regular::INTERSECT);
    assert_eq!(conflict_color, Color32::from_rgb(240, 78, 78));
}

#[test]
fn cloud_preview_provenance_uses_matching_in_thumbnail_badges() {
    use crate::cloud::CloudThumbnailKind::{Edited, Legacy, Placeholder, Raw};

    assert_eq!(cloud_preview_label(Edited), None);
    assert_eq!(cloud_preview_label(Raw), Some("UNEDITED PREVIEW"));
    assert_eq!(cloud_preview_label(Legacy), None);
    assert_eq!(
        cloud_preview_icon(Legacy),
        Some(egui_phosphor::regular::CLOCK_COUNTER_CLOCKWISE)
    );
    assert_eq!(cloud_preview_icon(Edited), None);
    assert_eq!(cloud_preview_label(Placeholder), Some("PREVIEW RENDERING"));
    assert!(cloud_preview_notice(Legacy).unwrap().contains("Legacy"));
}

#[test]
fn justified_rows_rebalance_to_avoid_a_sparse_last_row() {
    let aspects = vec![1.5; 13];
    let rows = balanced_justified_row_ranges(&aspects, 1000.0, 140.0, 6.0);
    let row_sizes = rows
        .iter()
        .map(|(start, end)| end - start)
        .collect::<Vec<_>>();

    assert_eq!(row_sizes.iter().sum::<usize>(), aspects.len());
    assert!(row_sizes.len() >= 2);
    assert!(row_sizes.iter().all(|count| *count >= 4));
    assert!(row_sizes.iter().all(|count| *count <= 5));
}

#[cfg(not(target_os = "android"))]
#[test]
fn sparse_galleries_never_grow_above_the_responsive_target() {
    for available_width in [320.0, 1024.0, 3440.0] {
        for target_height in [120.0, 140.0, 270.0] {
            for item_count in 1..=3 {
                let entries = (0..item_count)
                    .map(|index| {
                        new_library_entry(LibraryFileInfo {
                            source: LibrarySource::File(PathBuf::from(format!(
                                "sparse-{available_width}-{target_height}-{index}.dng"
                            ))),
                            display_path: format!("sparse-{index}.dng"),
                            name: format!("sparse-{index}.dng"),
                            bytes: 1,
                            dimensions_hint: Some([3, 2]),
                            cloud_downloaded: false,
                            cloud_sync_state: crate::cloud::CloudSyncState::Synced,
                            modified: None,
                        })
                    })
                    .collect::<Vec<_>>();

                let (placements, _) =
                    justified_thumbnail_layout(&entries, available_width, target_height, 6.0);

                assert_eq!(placements.len(), item_count);
                assert!(placements
                    .iter()
                    .all(|rect| rect.height() <= target_height + 0.01));
                assert!(placements
                    .iter()
                    .all(|rect| rect.width() <= target_height * 1.5 + 0.01));
                assert!(placements
                    .iter()
                    .all(|rect| rect.right() <= available_width + 0.01));
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
#[test]
fn decoded_preview_does_not_change_reserved_gallery_geometry() {
    let info = LibraryFileInfo {
        source: LibrarySource::File(PathBuf::from("stable-layout.dng")),
        display_path: "stable-layout.dng".to_owned(),
        name: "stable-layout.dng".to_owned(),
        bytes: 1,
        dimensions_hint: Some([6000, 4000]),
        cloud_downloaded: false,
        cloud_sync_state: crate::cloud::CloudSyncState::Synced,
        modified: None,
    };
    let mut entry = new_library_entry(info);
    let (before, before_height) =
        justified_thumbnail_layout(std::slice::from_ref(&entry), 900.0, 140.0, 6.0);

    // Embedded previews can have a slightly different crop/aspect. Loading
    // those pixels must not invalidate the geometry already reserved from
    // the RAW header.
    entry.thumbnail_size = Some([1600, 1200]);
    let (after, after_height) =
        justified_thumbnail_layout(std::slice::from_ref(&entry), 900.0, 140.0, 6.0);

    assert_eq!(before, after);
    assert_eq!(before_height, after_height);
}

#[cfg(not(target_os = "android"))]
#[test]
fn opening_a_library_folder_records_it_before_async_scanning() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "auraw-library-open-folder-{}-{nonce}",
        std::process::id()
    ));
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);

    // The path deliberately does not exist: the asynchronous scanner may
    // fail later, but the user's chosen location must be visible immediately.
    library.open_folder(root.clone(), &context);

    assert_eq!(library.folder(), Some(root.as_path()));
    assert_eq!(library.root_folder(), Some(root.as_path()));
}

#[cfg(not(target_os = "android"))]
#[test]
fn desktop_subfolder_navigation_keeps_the_chosen_root() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "auraw-library-navigation-{}-{nonce}",
        std::process::id()
    ));
    let nested = root.join("year").join("shoot");
    let outside = root.with_extension("outside");

    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);
    library.open_folder(root.clone(), &context);
    library.select_folder(nested.clone(), &context);

    assert_eq!(library.root_folder(), Some(root.as_path()));
    assert_eq!(library.folder(), Some(nested.as_path()));

    library.select_folder(outside.clone(), &context);
    assert_eq!(library.root_folder(), Some(root.as_path()));
    assert_eq!(library.folder(), Some(nested.as_path()));

    library.open_folder(root.clone(), &context);
    assert_eq!(library.root_folder(), Some(root.as_path()));
    assert_eq!(library.folder(), Some(root.as_path()));

    library.view = LibraryView::Cloud;
    assert!(library.select_folder(root.clone(), &context));
    assert_eq!(library.view, LibraryView::Local);
}

#[cfg(not(target_os = "android"))]
#[test]
fn restoring_a_library_reopens_and_reveals_its_selected_subfolder() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "auraw-library-restore-{}-{nonce}",
        std::process::id()
    ));
    let parent = root.join("year");
    let selected = parent.join("shoot");
    fs::create_dir_all(&selected).unwrap();
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);

    library.restore_folder(root.clone(), Some(selected.clone()), &context);

    assert_eq!(library.root_folder(), Some(root.as_path()));
    assert_eq!(library.folder(), Some(selected.as_path()));
    assert!(library.expanded_folders.contains(&root));
    assert!(library.expanded_folders.contains(&parent));
    assert!(library.expanded_folders.contains(&selected));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn restoring_library_navigation_reopens_the_saved_cloud_folder_and_tab() {
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);
    library.cloud_config = crate::cloud::CloudConfig {
        enabled: true,
        server_url: "http://127.0.0.1:1".to_owned(),
        access_token: String::new(),
    };
    let folder_id = "a".repeat(64);

    library.restore_navigation(LibraryView::Cloud, folder_id.clone(), &context);

    assert_eq!(library.view(), LibraryView::Cloud);
    assert_eq!(library.cloud_folder_id(), folder_id);
}

#[test]
fn restoring_invalid_cloud_navigation_falls_back_safely() {
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);

    library.restore_navigation(LibraryView::Cloud, "../outside".to_owned(), &context);

    assert_eq!(library.view(), LibraryView::Local);
    assert_eq!(
        library.cloud_folder_id(),
        crate::cloud::CLOUD_ROOT_FOLDER_ID
    );
}

#[cfg(not(target_os = "android"))]
#[test]
fn desktop_folder_tree_contains_nested_directories_and_ignores_symlinks() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before Unix epoch")
        .as_nanos();
    let root =
        std::env::temp_dir().join(format!("auraw-library-tree-{}-{nonce}", std::process::id()));
    fs::create_dir_all(root.join("Zulu").join("Nested")).unwrap();
    fs::create_dir_all(root.join("alpha")).unwrap();
    fs::write(root.join("not-a-folder.dng"), b"raw").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&root, root.join("Zulu").join("cycle")).unwrap();

    let tree = scan_folder_tree(&root, || false).expect("folder tree");
    let child_names = tree
        .children
        .iter()
        .map(|child| child.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(child_names, ["alpha", "Zulu"]);
    assert_eq!(tree.children[1].children[0].name, "Nested");
    #[cfg(unix)]
    assert_eq!(tree.children[1].children.len(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn evicted_thumbnail_restores_from_resident_pixels_without_reloading() {
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);
    let info = LibraryFileInfo {
        source: LibrarySource::File(PathBuf::from("resident-restore.dng")),
        display_path: "resident-restore.dng".to_owned(),
        name: "resident-restore.dng".to_owned(),
        bytes: 1,
        dimensions_hint: Some([6000, 4000]),
        cloud_downloaded: false,
        cloud_sync_state: crate::cloud::CloudSyncState::Synced,
        modified: None,
    };
    let mut entry = new_library_entry(info);
    entry.thumbnail_size = Some([512, 341]);
    entry.resident_thumbnail = Some(RawThumbnail {
        width: 384,
        height: 256,
        rgba: vec![127; 384 * 256 * 4],
    });
    library.entries.push(entry);

    library.touch_and_request_thumbnail(0, &context);

    assert!(library.entries[0].texture.is_some());
    assert!(library.entries[0].texture_is_resident);
    assert!(!library.entries[0].thumbnail_queued);
}

#[cfg(not(target_os = "android"))]
#[test]
fn develop_loading_thumbnail_uses_resident_pixels_without_queuing_decode() {
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);
    let path = PathBuf::from("develop-loading-resident.dng");
    let info = LibraryFileInfo {
        source: LibrarySource::File(path.clone()),
        display_path: "develop-loading-resident.dng".to_owned(),
        name: "develop-loading-resident.dng".to_owned(),
        bytes: 1,
        dimensions_hint: Some([6000, 4000]),
        cloud_downloaded: false,
        cloud_sync_state: crate::cloud::CloudSyncState::Synced,
        modified: None,
    };
    let mut entry = new_library_entry(info);
    entry.thumbnail_size = Some([512, 341]);
    entry.resident_thumbnail = Some(RawThumbnail {
        width: 384,
        height: 256,
        rgba: vec![127; 384 * 256 * 4],
    });
    library.entries.push(entry);
    library.rebuild_entry_indices();

    let (_, size) = library
        .desktop_loading_thumbnail_for_path(&path, &context)
        .expect("resident thumbnail");

    assert_eq!(size, [512, 341]);
    assert!(library.entries[0].texture_is_resident);
    assert!(!library.entries[0].thumbnail_queued);
}

#[cfg(not(target_os = "android"))]
#[test]
fn cloud_folder_entries_remain_available_to_the_develop_filmstrip() {
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);
    let folder_id = "a".repeat(64);
    let asset = crate::cloud::CloudAsset {
        id: "b".repeat(64),
        name: "folder-photo.NEF".to_owned(),
        bytes: 42,
        modified_seconds: 1,
        width: 6000,
        height: 4000,
        raw_etag: "c".repeat(64),
        sidecar_etag: Some("d".repeat(64)),
        thumbnail_etag: "e".repeat(64),
        thumbnail_kind: crate::cloud::CloudThumbnailKind::Edited,
        folder_id,
    };
    library.entries.push(new_library_entry(LibraryFileInfo {
        source: LibrarySource::Cloud(asset.clone()),
        display_path: "AuRaw Cloud/folder-photo.NEF".to_owned(),
        name: asset.name.clone(),
        bytes: asset.bytes,
        dimensions_hint: Some([asset.width, asset.height]),
        cloud_downloaded: false,
        cloud_sync_state: crate::cloud::CloudSyncState::Synced,
        modified: None,
    }));

    assert_eq!(library.filmstrip_len(), 1);
    let item = library.filmstrip_item(0).expect("cloud filmstrip item");
    assert!(matches!(
        item.source,
        super::DesktopFilmstripSource::Cloud(ref filmstrip_asset)
            if filmstrip_asset.id == asset.id
    ));
    assert!(item.path.is_none());
    assert_eq!(item.identity, format!("cloud:{}", asset.id));
}

#[cfg(not(target_os = "android"))]
#[test]
fn resetting_adjustments_allows_an_unedited_thumbnail_to_replace_the_developed_one() {
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);
    let path = PathBuf::from("reset-preview.dng");
    let info = LibraryFileInfo {
        source: LibrarySource::File(path.clone()),
        display_path: "reset-preview.dng".to_owned(),
        name: "reset-preview.dng".to_owned(),
        bytes: 1,
        dimensions_hint: Some([6000, 4000]),
        cloud_downloaded: false,
        cloud_sync_state: crate::cloud::CloudSyncState::Synced,
        modified: None,
    };
    let mut entry = new_library_entry(info);
    entry.texture = Some(context.load_texture(
        "developed-before-reset",
        eframe::egui::ColorImage::from_rgba_unmultiplied([1, 1], &[1, 2, 3, 255]),
        eframe::egui::TextureOptions::LINEAR,
    ));
    entry.resident_thumbnail = Some(RawThumbnail {
        width: 1,
        height: 1,
        rgba: vec![1, 2, 3, 255],
    });
    entry.thumbnail_size = Some([1, 1]);
    entry.thumbnail_queued = true;
    entry.developed_thumbnail = true;
    library.entries.push(entry);
    library.rebuild_entry_indices();

    library.invalidate_adjustment_thumbnail_for_path(&path);

    let entry = &library.entries[0];
    assert!(entry.texture.is_none());
    assert!(entry.resident_thumbnail.is_none());
    assert!(entry.thumbnail_size.is_none());
    assert!(!entry.thumbnail_queued);
    assert!(!entry.developed_thumbnail);
    assert_eq!(entry.layout_size, Some([6000, 4000]));
}

#[test]
fn resident_thumbnail_is_bounded_and_keeps_aspect_ratio() {
    let thumbnail = RawThumbnail {
        width: 768,
        height: 512,
        rgba: vec![255; 768 * 512 * 4],
    };
    let resident = make_resident_thumbnail(&thumbnail);
    assert_eq!([resident.width, resident.height], [384, 256]);
    assert_eq!(resident.rgba.len(), 384 * 256 * 4);
}

#[test]
fn develop_pause_preserves_a_received_non_priority_thumbnail_request() {
    let generation = 1;
    let cancellation = Arc::new(AtomicU64::new(generation));
    let decoding_paused = Arc::new(AtomicBool::new(true));
    let (event_sender, event_receiver) = mpsc::sync_channel(2);
    // A rendezvous channel makes send return only after the worker has
    // received the request, which proves it is retained during the pause.
    let (request_sender, request_receiver) = mpsc::sync_channel(0);
    let (decode_started_sender, decode_started_receiver) = mpsc::sync_channel(1);
    let worker_pause = Arc::clone(&decoding_paused);
    let worker = std::thread::spawn(move || {
        run_thumbnail_workers(
            ThumbnailWorker {
                files: Vec::new(),
                warning_count: 0,
                truncated: false,
                generation,
                cancellation,
                decoding_paused: worker_pause,
                decode_gate: Arc::new(RwLock::new(())),
                event_sender,
                request_receiver,
                repaint: eframe::egui::Context::default(),
            },
            1,
            Arc::new(move |_| {
                decode_started_sender.send(()).unwrap();
                Ok(loaded_library_thumbnail(
                    RawThumbnail {
                        width: 1,
                        height: 1,
                        rgba: vec![0, 0, 0, 255],
                    },
                    false,
                ))
            }),
        );
    });

    match event_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(ScanEvent::Catalog {
            generation: event_generation,
            ..
        }) => assert_eq!(event_generation, generation),
        _ => panic!("thumbnail worker did not publish its catalog"),
    }
    let source = LibrarySource::File(PathBuf::from("paused.dng"));
    request_sender
        .send(ThumbnailRequest {
            generation,
            source: source.clone(),
            display_priority: false,
        })
        .unwrap();
    assert!(matches!(
        decode_started_receiver.recv_timeout(Duration::from_millis(100)),
        Err(mpsc::RecvTimeoutError::Timeout)
    ));

    decoding_paused.store(false, Ordering::Release);
    decode_started_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("queued thumbnail should continue after resume");
    match event_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(ScanEvent::Thumbnail {
            generation: event_generation,
            source: event_source,
            display_priority,
            result: Ok(loaded),
        }) => {
            assert_eq!(event_generation, generation);
            assert_eq!(event_source, source);
            assert!(!display_priority);
            assert_eq!((loaded.thumbnail.width, loaded.thumbnail.height), (1, 1));
        }
        _ => panic!("thumbnail worker did not preserve the paused request"),
    }
    drop(request_sender);
    worker.join().unwrap();
}

#[test]
fn develop_pause_allows_display_priority_thumbnail_request() {
    let generation = 2;
    let cancellation = Arc::new(AtomicU64::new(generation));
    let decoding_paused = Arc::new(AtomicBool::new(true));
    let (event_sender, event_receiver) = mpsc::sync_channel(2);
    let (request_sender, request_receiver) = mpsc::sync_channel(0);
    let (decode_started_sender, decode_started_receiver) = mpsc::sync_channel(1);
    let worker_pause = Arc::clone(&decoding_paused);
    let worker = std::thread::spawn(move || {
        run_thumbnail_workers(
            ThumbnailWorker {
                files: Vec::new(),
                warning_count: 0,
                truncated: false,
                generation,
                cancellation,
                decoding_paused: worker_pause,
                decode_gate: Arc::new(RwLock::new(())),
                event_sender,
                request_receiver,
                repaint: eframe::egui::Context::default(),
            },
            1,
            Arc::new(move |_| {
                decode_started_sender.send(()).unwrap();
                Ok(loaded_library_thumbnail(
                    RawThumbnail {
                        width: 1,
                        height: 1,
                        rgba: vec![0, 0, 0, 255],
                    },
                    false,
                ))
            }),
        );
    });

    match event_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(ScanEvent::Catalog {
            generation: event_generation,
            ..
        }) => assert_eq!(event_generation, generation),
        _ => panic!("thumbnail worker did not publish its catalog"),
    }
    let source = LibrarySource::File(PathBuf::from("filmstrip.dng"));
    request_sender
        .send(ThumbnailRequest {
            generation,
            source: source.clone(),
            display_priority: true,
        })
        .unwrap();
    decode_started_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("display-priority thumbnail should run while Develop is paused");
    match event_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(ScanEvent::Thumbnail {
            generation: event_generation,
            source: event_source,
            display_priority,
            result: Ok(loaded),
        }) => {
            assert_eq!(event_generation, generation);
            assert_eq!(event_source, source);
            assert!(display_priority);
            assert_eq!((loaded.thumbnail.width, loaded.thumbnail.height), (1, 1));
        }
        _ => panic!("thumbnail worker did not service the display-priority request"),
    }
    drop(request_sender);
    worker.join().unwrap();
}

#[test]
fn thumbnail_workers_process_the_entire_catalog_without_view_requests() {
    let generation = 7;
    let files = ["one.dng", "two.dng", "three.dng"]
        .into_iter()
        .map(|name| LibraryFileInfo {
            source: LibrarySource::File(PathBuf::from(name)),
            display_path: name.to_owned(),
            name: name.to_owned(),
            bytes: 1,
            dimensions_hint: Some([3, 2]),
            cloud_downloaded: false,
            cloud_sync_state: crate::cloud::CloudSyncState::Synced,
            modified: None,
        })
        .collect::<Vec<_>>();
    let expected = files
        .iter()
        .map(|file| file.source.clone())
        .collect::<HashSet<_>>();
    let (event_sender, event_receiver) = mpsc::sync_channel(8);
    let (request_sender, request_receiver) = mpsc::sync_channel(1);
    drop(request_sender);
    let worker = std::thread::spawn(move || {
        run_thumbnail_workers(
            ThumbnailWorker {
                files,
                warning_count: 0,
                truncated: false,
                generation,
                cancellation: Arc::new(AtomicU64::new(generation)),
                decoding_paused: Arc::new(AtomicBool::new(false)),
                decode_gate: Arc::new(RwLock::new(())),
                event_sender,
                request_receiver,
                repaint: eframe::egui::Context::default(),
            },
            2,
            Arc::new(|_| {
                Ok(loaded_library_thumbnail(
                    RawThumbnail {
                        width: 1,
                        height: 1,
                        rgba: vec![0, 0, 0, 255],
                    },
                    false,
                ))
            }),
        );
    });

    assert!(matches!(
        event_receiver.recv_timeout(Duration::from_secs(2)),
        Ok(ScanEvent::Catalog { generation: 7, .. })
    ));
    let mut loaded = HashSet::new();
    for _ in 0..expected.len() {
        match event_receiver.recv_timeout(Duration::from_secs(2)) {
            Ok(ScanEvent::Thumbnail {
                generation: 7,
                source,
                display_priority,
                result: Ok(_),
            }) => {
                assert!(!display_priority);
                loaded.insert(source);
            }
            _ => panic!("thumbnail worker did not process the complete catalog"),
        }
    }
    assert_eq!(loaded, expected);
    worker.join().unwrap();
}

#[test]
fn library_exposes_its_shared_decode_gate_and_resumes_in_library() {
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new(&context);
    let first = library.decode_gate();
    let second = library.decode_gate();
    assert!(Arc::ptr_eq(&first, &second));

    library.prepare_for_develop();
    assert!(library.decoding_paused.load(Ordering::Acquire));
    library.resume_thumbnail_decoding();
    assert!(!library.decoding_paused.load(Ordering::Acquire));
}

#[cfg(unix)]
#[test]
fn non_utf8_paths_remain_distinct_library_keys() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    let first = PathBuf::from(OsString::from_vec(vec![b'r', b'a', b'w', 0x80]));
    let second = PathBuf::from(OsString::from_vec(vec![b'r', b'a', b'w', 0x81]));
    assert_eq!(first.display().to_string(), second.display().to_string());

    let sources = HashSet::from([LibrarySource::File(first), LibrarySource::File(second)]);
    assert_eq!(sources.len(), 2);
}

#[test]
fn duplicate_raw_copies_the_matching_sidecar_and_uses_unique_names() {
    let root = std::env::temp_dir().join(format!(
        "auraw-library-duplicate-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let raw = root.join("photo.CR3");
    fs::write(&raw, b"raw-bytes").unwrap();
    fs::write(crate::sidecar::sidecar_path_for_raw(&raw), b"sidecar-bytes").unwrap();
    install_test_developed_thumbnail(&raw);

    let first = duplicate_raw_and_sidecar(&raw).unwrap();
    let second = duplicate_raw_and_sidecar(&raw).unwrap();
    assert_eq!(first.file_name().unwrap(), "photo copy.CR3");
    assert_eq!(second.file_name().unwrap(), "photo copy 2.CR3");
    assert_eq!(fs::read(&first).unwrap(), b"raw-bytes");
    assert_eq!(
        fs::read(crate::sidecar::sidecar_path_for_raw(&first)).unwrap(),
        b"sidecar-bytes"
    );
    assert_test_developed_thumbnail(&first);
    assert_test_developed_thumbnail(&second);

    let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&raw);
    let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&first);
    let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&second);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_image_clipboard_copies_and_moves_raws_with_sidecars() {
    let root = std::env::temp_dir().join(format!(
        "auraw-library-image-clipboard-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&destination).unwrap();
    let raw = source.join("photo.CR3");
    fs::write(&raw, b"raw-bytes").unwrap();
    fs::write(crate::sidecar::sidecar_path_for_raw(&raw), b"sidecar-bytes").unwrap();
    install_test_developed_thumbnail(&raw);

    let copy = run_image_paste(
        &crate::cloud::CloudConfig::default(),
        None,
        false,
        ImageClipboard {
            mode: ImageClipboardMode::Copy,
            content: ImageClipboardContent::Local(vec![raw.clone()]),
        },
        ImagePasteDestination::LocalFolder(destination.clone()),
    );
    assert!(copy.result.is_ok());
    assert!(!copy.clear_clipboard);
    let copied = destination.join("photo.CR3");
    assert_eq!(fs::read(&copied).unwrap(), b"raw-bytes");
    assert_eq!(
        fs::read(crate::sidecar::sidecar_path_for_raw(&copied)).unwrap(),
        b"sidecar-bytes"
    );
    assert_test_developed_thumbnail(&copied);
    assert!(raw.is_file());

    let cut = run_image_paste(
        &crate::cloud::CloudConfig::default(),
        None,
        false,
        ImageClipboard {
            mode: ImageClipboardMode::Cut,
            content: ImageClipboardContent::Local(vec![raw.clone()]),
        },
        ImagePasteDestination::LocalFolder(destination.clone()),
    );
    assert!(cut.result.is_ok());
    assert!(cut.clear_clipboard);
    assert!(!raw.exists());
    assert!(!crate::sidecar::sidecar_path_for_raw(&raw).exists());
    let moved = destination.join("photo (1).CR3");
    assert_eq!(fs::read(&moved).unwrap(), b"raw-bytes");
    assert_eq!(
        fs::read(crate::sidecar::sidecar_path_for_raw(&moved)).unwrap(),
        b"sidecar-bytes"
    );
    assert_test_developed_thumbnail(&moved);

    let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&copied);
    let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&moved);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn partial_local_cut_keeps_only_unmoved_raws_on_the_clipboard() {
    let root = std::env::temp_dir().join(format!(
        "auraw-library-partial-cut-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&destination).unwrap();
    let moved = source.join("moved.CR3");
    let missing = source.join("missing.CR3");
    fs::write(&moved, b"raw").unwrap();

    let completion = run_image_paste(
        &crate::cloud::CloudConfig::default(),
        None,
        false,
        ImageClipboard {
            mode: ImageClipboardMode::Cut,
            content: ImageClipboardContent::Local(vec![moved.clone(), missing.clone()]),
        },
        ImagePasteDestination::LocalFolder(destination.clone()),
    );

    assert!(completion.result.is_err());
    assert!(!completion.clear_clipboard);
    assert!(!moved.exists());
    assert_eq!(fs::read(destination.join("moved.CR3")).unwrap(), b"raw");
    let remaining = completion.remaining_clipboard.unwrap();
    assert_eq!(remaining.count(), 1);
    match remaining.content {
        ImageClipboardContent::Local(paths) => assert_eq!(paths, vec![missing]),
        ImageClipboardContent::Cloud(_) => panic!("expected a local clipboard"),
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn local_raw_rename_keeps_the_matching_sidecar() {
    let root = std::env::temp_dir().join(format!(
        "auraw-library-rename-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let raw = root.join("before.NEF");
    fs::write(&raw, b"raw").unwrap();
    fs::write(crate::sidecar::sidecar_path_for_raw(&raw), b"sidecar").unwrap();
    install_test_developed_thumbnail(&raw);

    let renamed = rename_raw_bundle(&raw, "after.NEF").unwrap();
    assert_eq!(renamed, root.join("after.NEF"));
    assert!(!raw.exists());
    assert!(!crate::sidecar::sidecar_path_for_raw(&raw).exists());
    assert_eq!(fs::read(&renamed).unwrap(), b"raw");
    assert_eq!(
        fs::read(crate::sidecar::sidecar_path_for_raw(&renamed)).unwrap(),
        b"sidecar"
    );
    assert_test_developed_thumbnail(&renamed);
    assert!(crate::sidecar::load_developed_thumbnail_cache(&raw, 512)
        .unwrap()
        .is_none());

    let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&renamed);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn image_clipboard_uploads_local_raw_and_sidecar_to_cloud() {
    let root = std::env::temp_dir().join(format!(
        "auraw-library-local-cloud-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let raw = root.join("upload.DNG");
    let raw_bytes = b"clipboard-raw";
    let sidecar_bytes = b"clipboard-sidecar";
    fs::write(&raw, raw_bytes).unwrap();
    fs::write(crate::sidecar::sidecar_path_for_raw(&raw), sidecar_bytes).unwrap();
    install_test_developed_thumbnail(&raw);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let raw_etag = sha256_hex(raw_bytes);
    let sidecar_etag = sha256_hex(sidecar_bytes);
    let response = serde_json::to_vec(&serde_json::json!({
        "id": raw_etag,
        "name": "upload.DNG",
        "bytes": raw_bytes.len(),
        "modified_seconds": 1,
        "width": 32,
        "height": 24,
        "raw_etag": raw_etag,
        "sidecar_etag": sidecar_etag,
        "thumbnail_etag": "d".repeat(64),
        "folder_id": crate::cloud::CLOUD_ROOT_FOLDER_ID,
    }))
    .unwrap();
    let responder = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let request = read_http_request(&mut stream);
        let request_text = String::from_utf8_lossy(&request);
        assert!(request_text.starts_with("POST /api/v1/assets HTTP/1.1\r\n"));
        assert!(request_text.contains("name=\"raw\""));
        assert!(request_text.contains("clipboard-raw"));
        assert!(request_text.contains("name=\"sidecar\""));
        assert!(request_text.contains("clipboard-sidecar"));
        assert!(request_text.contains("name=\"thumbnail\""));
        write_http_response(&mut stream, "application/json", &response);
    });

    let completion = run_image_paste(
        &crate::cloud::CloudConfig {
            enabled: true,
            server_url: format!("http://{address}"),
            access_token: String::new(),
        },
        None,
        true,
        ImageClipboard {
            mode: ImageClipboardMode::Copy,
            content: ImageClipboardContent::Local(vec![raw.clone()]),
        },
        ImagePasteDestination::CloudFolder(crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned()),
    );
    responder.join().unwrap();
    assert!(completion.result.is_ok());
    assert!(raw.is_file());
    assert!(crate::sidecar::sidecar_path_for_raw(&raw).is_file());
    let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&raw);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn image_clipboard_downloads_cloud_raw_and_sidecar_to_local_folder() {
    let root = std::env::temp_dir().join(format!(
        "auraw-library-cloud-local-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let cache = root.join("cache");
    let destination = root.join("destination");
    fs::create_dir_all(&destination).unwrap();
    let raw = b"downloaded-cloud-raw";
    let sidecar = b"downloaded-cloud-sidecar";
    let thumbnail = test_developed_thumbnail_jpeg();
    let asset = crate::cloud::CloudAsset {
        id: sha256_hex(raw),
        name: "download.NEF".to_owned(),
        bytes: raw.len() as u64,
        modified_seconds: 1,
        width: 40,
        height: 30,
        raw_etag: sha256_hex(raw),
        sidecar_etag: Some(sha256_hex(sidecar)),
        thumbnail_etag: sha256_hex(&thumbnail),
        thumbnail_kind: crate::cloud::CloudThumbnailKind::Edited,
        folder_id: crate::cloud::CLOUD_ROOT_FOLDER_ID.to_owned(),
    };
    let catalog = serde_json::to_vec(&serde_json::json!({
        "items": [asset.clone()],
        "folders": [],
    }))
    .unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let expected_asset_id = asset.id.clone();
    let responder = std::thread::spawn(move || {
        for (path, content_type, body) in [
            ("/api/v1/assets".to_owned(), "application/json", catalog),
            (
                format!("/api/v1/assets/{expected_asset_id}/raw"),
                "application/octet-stream",
                raw.to_vec(),
            ),
            (
                format!("/api/v1/assets/{expected_asset_id}/sidecar"),
                "application/vnd.auraw.sidecar",
                sidecar.to_vec(),
            ),
            (
                format!(
                    "/api/v1/assets/{expected_asset_id}/thumbnail?v={}",
                    sha256_hex(&thumbnail)
                ),
                "image/jpeg",
                thumbnail,
            ),
        ] {
            let (mut stream, _) = listener.accept().unwrap();
            let request = read_http_request(&mut stream);
            assert!(String::from_utf8_lossy(&request)
                .starts_with(&format!("GET {path} HTTP/1.1\r\n")));
            write_http_response(&mut stream, content_type, &body);
        }
    });

    let completion = run_image_paste(
        &crate::cloud::CloudConfig {
            enabled: true,
            server_url: format!("http://{address}"),
            access_token: String::new(),
        },
        Some(&cache),
        true,
        ImageClipboard {
            mode: ImageClipboardMode::Copy,
            content: ImageClipboardContent::Cloud(vec![asset]),
        },
        ImagePasteDestination::LocalFolder(destination.clone()),
    );
    responder.join().unwrap();
    assert!(completion.result.is_ok(), "{:?}", completion.result);
    let copied = destination.join("download.NEF");
    assert_eq!(fs::read(&copied).unwrap(), raw);
    assert_eq!(
        fs::read(crate::sidecar::sidecar_path_for_raw(&copied)).unwrap(),
        sidecar
    );
    assert_test_developed_thumbnail(&copied);
    let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&copied);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_names_are_single_safe_path_components() {
    assert!(validate_folder_name("Photos 2026").is_ok());
    for invalid in ["", " ", ".", "..", "../outside", "nested/folder", "/tmp"] {
        assert!(
            validate_folder_name(invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
    #[cfg(windows)]
    assert!(validate_folder_name(r"nested\folder").is_err());
}

#[test]
fn recursive_folder_copy_never_overwrites_and_rejects_symlinks() {
    let root = std::env::temp_dir().join(format!(
        "auraw-library-folder-copy-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::write(source.join("nested").join("photo.dng"), b"raw").unwrap();

    copy_directory_create_new(&source, &destination).unwrap();
    assert_eq!(
        fs::read(destination.join("nested").join("photo.dng")).unwrap(),
        b"raw"
    );
    assert!(copy_directory_create_new(&source, &destination).is_err());

    #[cfg(unix)]
    {
        let linked_source = root.join("linked-source");
        let linked_destination = root.join("linked-destination");
        fs::create_dir(&linked_source).unwrap();
        std::os::unix::fs::symlink(&source, linked_source.join("link")).unwrap();
        assert!(copy_directory_create_new(&linked_source, &linked_destination).is_err());
        assert!(!linked_destination.exists());
    }

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_operations_stay_inside_the_library_and_protect_the_root() {
    let base = std::env::temp_dir().join(format!(
        "auraw-library-folder-boundary-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let root = base.join("library");
    let outside = base.join("outside");
    let source = root.join("source");
    let child = source.join("child");
    fs::create_dir_all(&child).unwrap();
    fs::create_dir(&outside).unwrap();

    assert!(run_folder_operation(LibraryFolderOperation::Create {
        root: root.clone(),
        parent: outside,
        name: "escape".to_owned(),
    })
    .is_err());
    assert!(run_folder_operation(LibraryFolderOperation::Delete {
        root: root.clone(),
        target: root.clone(),
    })
    .is_err());
    assert!(run_folder_operation(LibraryFolderOperation::Move {
        root: root.clone(),
        source,
        destination_parent: child,
        new_name: None,
    })
    .is_err());
    assert!(root.is_dir());

    fs::remove_dir_all(base).unwrap();
}

#[test]
fn dropped_folders_are_copied_recursively_with_unique_names() {
    let base = std::env::temp_dir().join(format!(
        "auraw-library-folder-import-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source = base.join("source").join("shoot");
    let library = base.join("library");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir(&library).unwrap();
    fs::write(source.join("photo.CR3"), b"raw").unwrap();

    let first = import_folder_into_library(&source, &library).unwrap();
    let second = import_folder_into_library(&source, &library).unwrap();
    assert_eq!(first.file_name().unwrap(), "shoot");
    assert_eq!(second.file_name().unwrap(), "shoot copy");
    assert_eq!(fs::read(first.join("photo.CR3")).unwrap(), b"raw");
    assert_eq!(fs::read(second.join("photo.CR3")).unwrap(), b"raw");

    fs::remove_dir_all(base).unwrap();
}

#[test]
fn dropped_raw_import_preserves_the_name_and_never_overwrites() {
    let root = std::env::temp_dir().join(format!(
        "auraw-library-import-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let source_folder = root.join("source");
    let library_folder = root.join("library");
    fs::create_dir_all(&source_folder).unwrap();
    fs::create_dir_all(&library_folder).unwrap();
    let source = source_folder.join("photo.CR3");
    fs::write(&source, b"new-raw").unwrap();

    let first = import_raw_into_folder(&source, &library_folder).unwrap();
    let first_path = match first {
        RawImportOutcome::Imported(path) => path,
        RawImportOutcome::AlreadyPresent => panic!("external source was not imported"),
    };
    assert_eq!(first_path.file_name().unwrap(), "photo.CR3");
    assert_eq!(fs::read(&first_path).unwrap(), b"new-raw");

    fs::write(&source, b"newer-raw").unwrap();
    let second = import_raw_into_folder(&source, &library_folder).unwrap();
    let second_path = match second {
        RawImportOutcome::Imported(path) => path,
        RawImportOutcome::AlreadyPresent => panic!("changed external source was not imported"),
    };
    assert_eq!(second_path.file_name().unwrap(), "photo (1).CR3");
    assert_eq!(fs::read(&first_path).unwrap(), b"new-raw");
    assert_eq!(fs::read(&second_path).unwrap(), b"newer-raw");

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn dropping_a_raw_already_in_the_library_is_a_noop() {
    let root = std::env::temp_dir().join(format!(
        "auraw-library-import-noop-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let raw = root.join("photo.DNG");
    fs::write(&raw, b"raw").unwrap();

    assert!(matches!(
        import_raw_into_folder(&raw, &root).unwrap(),
        RawImportOutcome::AlreadyPresent
    ));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_scan_only_includes_direct_raw_children() {
    let root = std::env::temp_dir().join(format!(
        "auraw-library-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let nested = root.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(root.join("one.DNG"), b"raw").unwrap();
    fs::write(nested.join("two.nef"), b"raw").unwrap();
    fs::write(root.join("ignore.jpg"), b"jpeg").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&root, nested.join("cycle")).unwrap();

    let (files, warnings, truncated) = scan_folder(&root, || false).unwrap().unwrap();
    let names = files
        .iter()
        .map(|file| file.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(warnings, 0);
    assert!(!truncated);
    assert!(names.contains(&"one.DNG"));
    assert!(!names.contains(&"two.nef"));
    assert_eq!(files.len(), 1);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn folder_scan_retains_newest_files_after_reaching_limit() {
    use std::time::Duration;

    let root = std::env::temp_dir().join(format!(
        "auraw-library-limit-test-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    let epoch = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    for (name, age) in [
        ("oldest.dng", 1),
        ("newest.dng", 5),
        ("middle.dng", 3),
        ("older.dng", 2),
        ("newer.dng", 4),
    ] {
        let path = root.join(name);
        fs::write(&path, b"raw").unwrap();
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_times(fs::FileTimes::new().set_modified(epoch + Duration::from_secs(age)))
            .unwrap();
    }

    let (files, warnings, truncated) =
        scan_folder_with_limit(&root, 3, || false).unwrap().unwrap();
    let names = files
        .iter()
        .map(|file| file.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["newest.dng", "newer.dng", "middle.dng"]);
    assert_eq!(warnings, 0);
    assert!(truncated);
    fs::remove_dir_all(root).unwrap();
}

