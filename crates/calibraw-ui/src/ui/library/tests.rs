use super::state::{library_filename_matches, library_search_terms};
use super::*;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

fn unique_temp_dir(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "calibraw-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    root
}

#[cfg(not(target_os = "android"))]
#[test]
fn library_export_naming_preserves_format_and_avoids_collisions() {
    let root = unique_temp_dir("library-export-name-test");
    let source = root.join("photo.CR3");
    let existing = root.join("photo-calibraw.jpg");
    fs::write(&source, b"raw").unwrap();
    fs::write(&existing, b"existing").unwrap();
    let mut reserved = HashSet::new();

    let destination = super::export::unique_library_export_path(
        &root,
        &source,
        ExportFormat::Jpeg,
        &mut reserved,
    );

    assert_eq!(destination, root.join("photo-calibraw-2.jpg"));
    assert!(reserved.contains(&destination));
    fs::remove_dir_all(root).unwrap();
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
fn install_test_developed_thumbnail(raw: &Path) {
    let fingerprint = crate::sidecar::desktop_sidecar_fingerprint(raw)
        .unwrap()
        .expect("test RAW should have an edit sidecar");
    crate::sidecar::save_developed_thumbnail_cache(raw, &test_developed_thumbnail(), fingerprint)
        .unwrap();
}

#[cfg(not(target_os = "android"))]
fn assert_test_developed_thumbnail(raw: &Path) {
    let thumbnail = crate::sidecar::load_developed_thumbnail_cache(raw, 512)
        .unwrap()
        .expect("copied RAW should retain its developed thumbnail");
    assert_eq!([thumbnail.width, thumbnail.height], [16, 12]);
    let pixel = &thumbnail.rgba[..4];
    assert!(pixel[2] > pixel[1] && pixel[1] > pixel[0], "{pixel:?}");
}

#[cfg(not(target_os = "android"))]
fn test_asset(path: impl Into<PathBuf>) -> LibraryAsset {
    LibraryAsset::from_desktop_path(path.into(), 42, 123, Some([6000, 4000]))
}

#[cfg(target_os = "android")]
fn test_asset(name: impl Into<PathBuf>) -> LibraryAsset {
    let name = name.into().display().to_string();
    let uri = format!("content://calibraw.test/{name}");
    LibraryAsset {
        id: LibraryAssetId::Android(uri.clone()),
        display_name: name.clone(),
        display_path: format!("Library/{name}"),
        locator: LibraryLocator::Android { uri },
        metadata: LibraryAssetMetadata {
            bytes: 42,
            dimensions_hint: Some([6000, 4000]),
            modified_seconds: 123,
        },
    }
}

#[test]
fn android_folder_navigation_uses_normalized_relative_paths() {
    assert_eq!(android_folder_parent(""), "");
    assert_eq!(android_folder_parent("Trips"), "");
    assert_eq!(android_folder_parent("Trips/Day 1"), "Trips");
    assert_eq!(
        android_library_location_label("Android/media/de.duecki.calibraw/.library", "Trips/Day 1"),
        "Android/media/de.duecki.calibraw/.library/Trips/Day 1"
    );
    let ancestors = android_folder_ancestors("Trips/Day 1");
    assert!(ancestors.contains(""));
    assert!(ancestors.contains("Trips"));
}

#[test]
fn unified_asset_keeps_identity_locator_and_metadata_together() {
    let asset = test_asset("photo.CR3");
    assert_eq!(asset.display_name, "photo.CR3");
    assert_eq!(asset.metadata.bytes, 42);
    assert_eq!(asset.metadata.dimensions_hint, Some([6000, 4000]));
    assert_eq!(asset.metadata.modified_seconds, 123);

    #[cfg(not(target_os = "android"))]
    assert_eq!(asset.desktop_path(), Some(Path::new("photo.CR3")));
    #[cfg(target_os = "android")]
    assert!(asset
        .android_uri()
        .is_some_and(|uri| uri.starts_with("content://")));
}

#[test]
fn selection_actions_are_shared_and_single_item_actions_stay_guarded() {
    let one = vec![test_asset("one.CR3")];
    let two = vec![test_asset("one.CR3"), test_asset("two.NEF")];

    assert!(matches!(
        library_selection_action(SelectionBarCommand::Rename, &one),
        Some(LibraryAction::Rename(_))
    ));
    assert!(library_selection_action(SelectionBarCommand::Rename, &two).is_none());
    assert!(matches!(
        library_selection_action(SelectionBarCommand::Copy, &two),
        Some(LibraryAction::Copy(assets)) if assets.len() == 2
    ));
    assert!(matches!(
        library_selection_action(SelectionBarCommand::Delete, &two),
        Some(LibraryAction::Delete(assets)) if assets.len() == 2
    ));
}

#[cfg(not(target_os = "android"))]
#[test]
fn thumbnail_selection_uses_unified_asset_ids() {
    let mut library = LibraryState::new();
    let first = LibraryAssetId::Desktop(PathBuf::from("one.CR3"));
    let second = LibraryAssetId::Desktop(PathBuf::from("two.NEF"));

    assert!(!library.selection_mode());
    assert!(library.toggle_thumbnail_selection(&first));
    assert!(library.has_selection());
    assert!(library.toggle_thumbnail_selection(&second));
    assert!(library.toggle_thumbnail_selection(&first));
    assert!(!library.toggle_thumbnail_selection(&second));
    assert!(!library.has_selection());
    assert!(!library.selection_mode());
}

#[test]
fn filename_search_is_case_insensitive_and_supports_comma_separated_fragments() {
    let terms = library_search_terms(" DSC23824, dsc384384 , ");

    assert!(library_filename_matches("DSC23824.CR3", &terms));
    assert!(library_filename_matches("Dsc384384.NEF", &terms));
    assert!(!library_filename_matches("DSC00001.ARW", &terms));
    assert!(library_filename_matches(
        "anything.dng",
        &library_search_terms("  ")
    ));
}

#[cfg(not(target_os = "android"))]
#[test]
fn selecting_search_matches_replaces_the_selection_with_every_visible_match() {
    let mut library = LibraryState::new();
    library.entries = [
        test_asset("DSC23824.CR3"),
        test_asset("DSC384384.NEF"),
        test_asset("holiday.ARW"),
    ]
    .into_iter()
    .map(new_library_entry)
    .collect();
    library.selected_assets.insert(test_asset("holiday.ARW").id);
    library.selection_mode = true;
    *library.search_query_mut() = "dsc23824, DSC384384".to_owned();

    assert_eq!(library.filtered_entry_indices(), vec![0, 1]);
    assert_eq!(library.select_search_matches(), 2);
    assert_eq!(library.selected_assets.len(), 2);
    assert!(library
        .selected_assets
        .contains(&test_asset("DSC23824.CR3").id));
    assert!(library
        .selected_assets
        .contains(&test_asset("DSC384384.NEF").id));
    assert!(!library
        .selected_assets
        .contains(&test_asset("holiday.ARW").id));
}

#[test]
fn catalog_status_only_reports_exceptional_conditions() {
    assert_eq!(catalog_status(0, false), "");
    assert_eq!(catalog_status(1, false), "1 unreadable item");
    assert!(catalog_status(0, true).contains("Newest"));
    let combined = catalog_status(2, true);
    assert!(combined.contains("Newest"));
    assert!(combined.contains("2 unreadable items"));
}

#[test]
fn justified_thumbnail_grid_preserves_image_aspect_ratios_per_row() {
    let mut entries = [test_asset("portrait.CR3"), test_asset("landscape.CR3")]
        .into_iter()
        .map(new_library_entry)
        .collect::<Vec<_>>();
    entries[0].layout_size = Some([2, 3]);
    entries[1].layout_size = Some([3, 2]);

    let (tiles, total_height) = justified_thumbnail_layout(&entries, 368.0, 150.0, 6.0);

    assert_eq!(tiles.len(), 2);
    assert_eq!(tiles[0].top(), tiles[1].top());
    assert_eq!(tiles[0].height(), tiles[1].height());
    assert!(tiles[1].width() > tiles[0].width());
    assert!(tiles.last().unwrap().right() < 368.0);
    assert_eq!(total_height, tiles[0].height());
}

#[test]
fn thumbnail_background_progress_is_generation_scoped_and_deduplicated() {
    let first = test_asset("one.CR3").id;
    let second = test_asset("two.NEF").id;
    let mut progress = ThumbnailBackgroundProgress::default();
    progress.begin(12, 2);
    progress.record_completion(11, first.clone());
    progress.record_completion(12, first.clone());
    progress.record_completion(12, first);

    assert_eq!(
        progress.snapshot(false),
        Some(ThumbnailProgress {
            completed: 1,
            total: 2,
            paused: false,
        })
    );

    progress.record_completion(12, second);
    assert_eq!(progress.snapshot(false), None);
}

#[test]
fn middle_elision_is_readable() {
    let elided = elide_middle("0123456789abcdefghij", 11);
    assert!(elided.starts_with("01234"));
    assert!(elided.ends_with("ghij"));
    assert!(elided.contains('…'));
}

#[test]
fn thumbnail_size_and_responsive_mobile_target_remain_stable() {
    assert_eq!(
        LibraryThumbnailSize::default(),
        LibraryThumbnailSize::Medium
    );
    assert_eq!(LibraryThumbnailSize::Small.scale(), 1.0);
    assert!(LibraryThumbnailSize::Large.scale() > LibraryThumbnailSize::Medium.scale());
    assert_eq!(
        responsive_thumbnail_target_height(400.0, 800.0, 3.0, true),
        120.0
    );
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
        bounds.right_bottom() - eframe::egui::Vec2::splat(crate::ui::theme::FLOATING_ACTION_MARGIN)
    );
    assert_eq!(library_import_icon(), egui_phosphor::regular::PLUS);
}

#[test]
fn complete_rows_fill_the_viewport_and_the_final_row_is_left_sparse() {
    let aspects = vec![1.5; 7];
    let rows = justified_thumbnail_row_ranges(&aspects, 640.0, 140.0, 6.0);

    assert_eq!(rows, vec![(0, 3), (3, 6), (6, 7)]);
}

#[test]
fn sparse_gallery_does_not_stretch_above_target_height() {
    let assets = [test_asset("one.CR3"), test_asset("two.CR3")];
    let entries = assets
        .into_iter()
        .map(new_library_entry)
        .collect::<Vec<_>>();
    let (rects, height) = justified_thumbnail_layout(&entries, 1200.0, 160.0, 8.0);
    assert_eq!(rects.len(), 2);
    assert!(rects
        .iter()
        .all(|rect| rect.height() <= 160.0 + f32::EPSILON));
    assert!(height <= 160.0 + f32::EPSILON);
    assert!(rects.last().unwrap().right() < 1200.0);
}

#[test]
fn sparse_row_keeps_the_selected_thumbnail_height() {
    let assets = [test_asset("portrait.CR3"), test_asset("landscape.CR3")];
    let mut entries = assets
        .into_iter()
        .map(new_library_entry)
        .collect::<Vec<_>>();
    entries[0].layout_size = Some([2, 3]);
    entries[1].layout_size = Some([3, 2]);

    let available_width = 420.0;
    let (small_rects, small_height) =
        justified_thumbnail_layout(&entries, available_width, 120.0, 6.0);
    let (large_rects, large_height) =
        justified_thumbnail_layout(&entries, available_width, 180.0, 6.0);

    assert_eq!(small_rects.len(), 2);
    assert_eq!(small_height, 120.0);
    assert_eq!(large_height, 180.0);
    assert!(small_rects.last().unwrap().right() < available_width);
    assert!(large_rects[0].width() > small_rects[0].width());
}

#[test]
fn thumbnail_hover_details_include_format_and_dimensions() {
    let asset = test_asset("portrait.CR3");

    assert_eq!(thumbnail_hover_details(&asset), "CR3  ·  6000 × 4000");
}

#[test]
fn cover_uv_crops_without_leaving_unit_square() {
    let uv = thumbnail_cover_uv(Some([2000, 1000]), egui::vec2(100.0, 100.0));
    assert!(uv.left() > 0.0);
    assert!(uv.right() < 1.0);
    assert_eq!(uv.top(), 0.0);
    assert_eq!(uv.bottom(), 1.0);
}

#[test]
fn image_paste_summary_is_platform_neutral() {
    assert_eq!(
        image_paste_summary(ImageClipboardMode::Copy, 2, 2, "destination", Vec::new()).unwrap(),
        "Copied 2 RAWs to destination."
    );
    let error = image_paste_summary(
        ImageClipboardMode::Cut,
        2,
        1,
        "destination",
        vec!["two.NEF: denied".to_owned()],
    )
    .unwrap_err();
    assert!(error.contains("Moved 1 of 2 RAWs"));
    assert!(error.contains("denied"));
}

#[test]
fn raw_names_require_safe_supported_extensions() {
    assert!(validate_library_item_name("photo.CR3", true).is_ok());
    for invalid in ["", " photo.CR3", "nested/photo.CR3", "photo.jpg"] {
        assert!(
            validate_library_item_name(invalid, true).is_err(),
            "accepted {invalid:?}"
        );
    }
}

#[cfg(all(not(target_os = "android"), unix))]
#[test]
fn non_utf8_paths_remain_distinct_asset_ids() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let first = PathBuf::from(OsString::from_vec(vec![b'r', b'a', b'w', 0x80]));
    let second = PathBuf::from(OsString::from_vec(vec![b'r', b'a', b'w', 0x81]));
    assert_eq!(first.display().to_string(), second.display().to_string());
    let ids = std::collections::HashSet::from([
        LibraryAssetId::Desktop(first),
        LibraryAssetId::Desktop(second),
    ]);
    assert_eq!(ids.len(), 2);
}

#[cfg(not(target_os = "android"))]
#[test]
fn decoded_preview_does_not_change_reserved_gallery_geometry() {
    let mut entry = new_library_entry(test_asset("stable-layout.dng"));
    let (before, before_height) =
        justified_thumbnail_layout(std::slice::from_ref(&entry), 900.0, 140.0, 6.0);

    entry.thumbnail_size = Some([1600, 1200]);
    let (after, after_height) =
        justified_thumbnail_layout(std::slice::from_ref(&entry), 900.0, 140.0, 6.0);

    assert_eq!(before, after);
    assert_eq!(before_height, after_height);
}

#[cfg(not(target_os = "android"))]
#[test]
fn opening_a_library_folder_records_it_before_async_scanning() {
    let root = unique_temp_dir("library-open-folder").join("not-created");
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new();

    library.open_folder(root.clone(), &context);

    assert_eq!(library.folder(), Some(root.as_path()));
    assert_eq!(library.root_folder(), Some(root.as_path()));
    fs::remove_dir_all(root.parent().unwrap()).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn desktop_subfolder_navigation_keeps_the_chosen_root() {
    let root = unique_temp_dir("library-navigation");
    let nested = root.join("year").join("shoot");
    let outside = root.with_extension("outside");
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new();

    library.open_folder(root.clone(), &context);
    assert!(library.select_folder(nested.clone(), &context));
    assert_eq!(library.root_folder(), Some(root.as_path()));
    assert_eq!(library.folder(), Some(nested.as_path()));

    assert!(!library.select_folder(outside, &context));
    assert_eq!(library.root_folder(), Some(root.as_path()));
    assert_eq!(library.folder(), Some(nested.as_path()));

    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn restoring_a_library_reopens_and_reveals_its_selected_subfolder() {
    let root = unique_temp_dir("library-restore");
    let parent = root.join("year");
    let selected = parent.join("shoot");
    fs::create_dir_all(&selected).unwrap();
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new();

    library.restore_folder(root.clone(), Some(selected.clone()), &context);

    assert_eq!(library.root_folder(), Some(root.as_path()));
    assert_eq!(library.folder(), Some(selected.as_path()));
    assert!(library.expanded_folders.contains(&root));
    assert!(library.expanded_folders.contains(&parent));
    assert!(library.expanded_folders.contains(&selected));

    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn desktop_folder_tree_contains_nested_directories_and_ignores_symlinks() {
    let root = unique_temp_dir("library-tree");
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
    let mut library = LibraryState::new();
    let mut entry = new_library_entry(test_asset("resident-restore.dng"));
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
    let mut library = LibraryState::new();
    let path = PathBuf::from("develop-loading-resident.dng");
    let mut entry = new_library_entry(test_asset(path.clone()));
    entry.thumbnail_size = Some([512, 341]);
    entry.resident_thumbnail = Some(RawThumbnail {
        width: 384,
        height: 256,
        rgba: vec![127; 384 * 256 * 4],
    });
    library.entries.push(entry);
    library.rebuild_entry_indices();

    let (_, size, adaptive_backdrop) = library
        .desktop_loading_thumbnail_for_path(&path, &context)
        .expect("resident thumbnail");

    assert_eq!(size, [512, 341]);
    assert!(adaptive_backdrop.is_some());
    assert!(library.entries[0].texture_is_resident);
    assert!(!library.entries[0].thumbnail_queued);
}

#[cfg(not(target_os = "android"))]
#[test]
fn resetting_adjustments_allows_an_unedited_thumbnail_to_replace_the_developed_one() {
    let context = eframe::egui::Context::default();
    let mut library = LibraryState::new();
    let path = PathBuf::from("reset-preview.dng");
    let mut entry = new_library_entry(test_asset(path.clone()));
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

    library.invalidate_adjustment_thumbnail_for_asset(&test_asset(path));

    let entry = &library.entries[0];
    assert!(entry.texture.is_none());
    assert!(entry.resident_thumbnail.is_none());
    assert!(entry.thumbnail_size.is_none());
    assert!(!entry.thumbnail_queued);
    assert!(!entry.developed_thumbnail);
    assert_eq!(entry.layout_size, Some([6000, 4000]));
}

#[test]
fn develop_pause_preserves_a_received_non_priority_thumbnail_request() {
    let generation = 1;
    let asset = test_asset("paused.dng");
    let asset_id = asset.id.clone();
    let cancellation = Arc::new(AtomicU64::new(generation));
    let decoding_paused = Arc::new(AtomicBool::new(true));
    let (event_sender, event_receiver) = mpsc::sync_channel(2);
    let (request_sender, request_receiver) = mpsc::sync_channel(0);
    let (decode_started_sender, decode_started_receiver) = mpsc::sync_channel(1);
    let worker_pause = Arc::clone(&decoding_paused);
    let worker = std::thread::spawn(move || {
        run_thumbnail_workers(
            ThumbnailWorker {
                assets: vec![asset],
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
            Arc::new(move |_, _| {
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

    assert!(matches!(
        event_receiver.recv_timeout(Duration::from_secs(2)),
        Ok(ScanEvent::Catalog {
            generation: event_generation,
            ..
        }) if event_generation == generation
    ));
    request_sender
        .send(ThumbnailRequest {
            generation,
            asset_id: asset_id.clone(),
            display_priority: false,
            stage: ThumbnailLoadStage::RawPreview,
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
            asset_id: event_asset_id,
            display_priority,
            final_thumbnail: true,
            result: Ok(loaded),
        }) => {
            assert_eq!(event_generation, generation);
            assert_eq!(event_asset_id, asset_id);
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
    let asset = test_asset("filmstrip.dng");
    let asset_id = asset.id.clone();
    let cancellation = Arc::new(AtomicU64::new(generation));
    let decoding_paused = Arc::new(AtomicBool::new(true));
    let (event_sender, event_receiver) = mpsc::sync_channel(2);
    let (request_sender, request_receiver) = mpsc::sync_channel(0);
    let (decode_started_sender, decode_started_receiver) = mpsc::sync_channel(1);
    let worker_pause = Arc::clone(&decoding_paused);
    let worker = std::thread::spawn(move || {
        run_thumbnail_workers(
            ThumbnailWorker {
                assets: vec![asset],
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
            Arc::new(move |_, _| {
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

    assert!(matches!(
        event_receiver.recv_timeout(Duration::from_secs(2)),
        Ok(ScanEvent::Catalog {
            generation: event_generation,
            ..
        }) if event_generation == generation
    ));
    request_sender
        .send(ThumbnailRequest {
            generation,
            asset_id: asset_id.clone(),
            display_priority: true,
            stage: ThumbnailLoadStage::RawPreview,
        })
        .unwrap();
    decode_started_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("display-priority thumbnail should run while Develop is paused");
    match event_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(ScanEvent::Thumbnail {
            generation: event_generation,
            asset_id: event_asset_id,
            display_priority,
            final_thumbnail: true,
            result: Ok(loaded),
        }) => {
            assert_eq!(event_generation, generation);
            assert_eq!(event_asset_id, asset_id);
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
    let assets = ["one.dng", "two.dng", "three.dng"]
        .into_iter()
        .map(test_asset)
        .collect::<Vec<_>>();
    let expected = assets
        .iter()
        .map(|asset| asset.id.clone())
        .collect::<HashSet<_>>();
    let (event_sender, event_receiver) = mpsc::sync_channel(8);
    let (request_sender, request_receiver) = mpsc::sync_channel(1);
    drop(request_sender);
    let worker = std::thread::spawn(move || {
        run_thumbnail_workers(
            ThumbnailWorker {
                assets,
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
            Arc::new(|_, _| {
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
                asset_id,
                display_priority,
                final_thumbnail: true,
                result: Ok(_),
            }) => {
                assert!(!display_priority);
                loaded.insert(asset_id);
            }
            _ => panic!("thumbnail worker did not process the complete catalog"),
        }
    }
    assert_eq!(loaded, expected);
    worker.join().unwrap();
}

#[test]
fn edited_thumbnail_workers_publish_raw_preview_before_background_development() {
    let generation = 9;
    let asset = test_asset("edited-preview.dng");
    let (event_sender, event_receiver) = mpsc::sync_channel(4);
    let (request_sender, request_receiver) = mpsc::sync_channel(1);
    drop(request_sender);
    let (stage_sender, stage_receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        run_thumbnail_workers(
            ThumbnailWorker {
                assets: vec![asset],
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
            1,
            Arc::new(move |_, stage| {
                stage_sender.send(stage).unwrap();
                let thumbnail = RawThumbnail {
                    width: 1,
                    height: 1,
                    rgba: vec![0, 0, 0, 255],
                };
                Ok(match stage {
                    ThumbnailLoadStage::RawPreview => {
                        loaded_library_raw_preview_pending_development(thumbnail)
                    }
                    ThumbnailLoadStage::DevelopedPreview => {
                        loaded_library_thumbnail(thumbnail, true)
                    }
                })
            }),
        );
    });

    assert!(matches!(
        event_receiver.recv_timeout(Duration::from_secs(2)),
        Ok(ScanEvent::Catalog { generation: 9, .. })
    ));
    match event_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(ScanEvent::Thumbnail {
            final_thumbnail: false,
            result: Ok(loaded),
            ..
        }) => {
            assert!(!loaded.developed);
            assert!(loaded.developed_thumbnail_stale);
            assert!(loaded.developed_render_pending);
        }
        _ => panic!("worker did not publish the RAW preview first"),
    }
    match event_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(ScanEvent::Thumbnail {
            final_thumbnail: true,
            result: Ok(loaded),
            ..
        }) => {
            assert!(loaded.developed);
            assert!(!loaded.developed_thumbnail_stale);
            assert!(!loaded.developed_render_pending);
        }
        _ => panic!("worker did not replace the RAW preview with its edited thumbnail"),
    }
    assert_eq!(
        stage_receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
        ThumbnailLoadStage::RawPreview
    );
    assert_eq!(
        stage_receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
        ThumbnailLoadStage::DevelopedPreview
    );
    worker.join().unwrap();
}

#[test]
fn stale_edited_preview_does_not_schedule_development_when_index_rendering_is_disabled() {
    let generation = 10;
    let asset = test_asset("stale-edited-preview.dng");
    let (event_sender, event_receiver) = mpsc::sync_channel(3);
    let (request_sender, request_receiver) = mpsc::sync_channel(1);
    drop(request_sender);
    let (stage_sender, stage_receiver) = mpsc::channel();
    let worker = std::thread::spawn(move || {
        run_thumbnail_workers(
            ThumbnailWorker {
                assets: vec![asset],
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
            1,
            Arc::new(move |_, stage| {
                stage_sender.send(stage).unwrap();
                Ok(loaded_library_raw_preview_with_stale_edits(RawThumbnail {
                    width: 1,
                    height: 1,
                    rgba: vec![0, 0, 0, 255],
                }))
            }),
        );
    });

    assert!(matches!(
        event_receiver.recv_timeout(Duration::from_secs(2)),
        Ok(ScanEvent::Catalog { generation: 10, .. })
    ));
    match event_receiver.recv_timeout(Duration::from_secs(2)) {
        Ok(ScanEvent::Thumbnail {
            final_thumbnail: true,
            result: Ok(loaded),
            ..
        }) => {
            assert!(!loaded.developed);
            assert!(loaded.developed_thumbnail_stale);
            assert!(!loaded.developed_render_pending);
        }
        _ => panic!("worker did not publish the stale original preview as final"),
    }
    assert_eq!(
        stage_receiver.recv_timeout(Duration::from_secs(2)).unwrap(),
        ThumbnailLoadStage::RawPreview
    );
    assert!(stage_receiver
        .recv_timeout(Duration::from_millis(100))
        .is_err());
    worker.join().unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn disabled_index_rendering_still_reuses_a_fresh_edited_thumbnail() {
    let root = unique_temp_dir("library-fresh-edited-thumbnail");
    let raw = root.join("edited.dng");
    fs::write(&raw, b"raw").unwrap();
    fs::write(crate::sidecar::sidecar_path_for_raw(&raw), b"saved edits").unwrap();
    install_test_developed_thumbnail(&raw);

    let loaded =
        load_desktop_library_thumbnail(&test_asset(raw), ThumbnailLoadStage::RawPreview, false)
            .expect(
                "a fresh edited thumbnail should remain usable when index rendering is disabled",
            );

    assert!(loaded.developed);
    assert!(!loaded.developed_thumbnail_stale);
    assert!(!loaded.developed_render_pending);
    assert_eq!([loaded.thumbnail.width, loaded.thumbnail.height], [16, 12]);
    fs::remove_dir_all(root).unwrap();
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

#[cfg(not(target_os = "android"))]
#[test]
fn library_exposes_its_shared_decode_gate_and_resumes_in_library() {
    let mut library = LibraryState::new();
    let first = library.decode_gate();
    let second = library.decode_gate();
    assert!(Arc::ptr_eq(&first, &second));

    library.prepare_for_develop();
    assert!(library.decoding_paused.load(Ordering::Acquire));
    library.resume_thumbnail_decoding();
    assert!(!library.decoding_paused.load(Ordering::Acquire));
}

#[cfg(not(target_os = "android"))]
#[test]
fn dropped_folders_are_copied_recursively_with_unique_names() {
    let base = unique_temp_dir("library-folder-import");
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

#[cfg(not(target_os = "android"))]
#[test]
fn dropping_a_raw_already_in_the_library_is_a_noop() {
    let root = unique_temp_dir("library-import-noop");
    let raw = root.join("photo.DNG");
    fs::write(&raw, b"raw").unwrap();

    assert!(matches!(
        import_raw_into_folder(&raw, &root).unwrap(),
        RawImportOutcome::AlreadyPresent
    ));
    assert_eq!(fs::read_dir(&root).unwrap().count(), 1);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn shared_clipboard_flow_copies_and_moves_raw_sidecar_bundles() {
    let root = unique_temp_dir("library-clipboard-test");
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&destination).unwrap();
    let raw = source.join("photo.CR3");
    fs::write(&raw, b"raw-bytes").unwrap();
    fs::write(crate::sidecar::sidecar_path_for_raw(&raw), b"sidecar-bytes").unwrap();
    install_test_developed_thumbnail(&raw);

    let asset = LibraryAsset::from_desktop_path(raw.clone(), 9, 1, None);
    let copy = run_image_paste(
        ImageClipboard {
            mode: ImageClipboardMode::Copy,
            assets: vec![asset.clone()],
        },
        LibraryTransferDestination::LocalFolder(destination.clone()),
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
    assert!(raw.exists());

    let cut = run_image_paste(
        ImageClipboard {
            mode: ImageClipboardMode::Cut,
            assets: vec![asset],
        },
        LibraryTransferDestination::LocalFolder(destination.clone()),
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

    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn duplicate_uses_shared_materialize_import_flow() {
    let root = unique_temp_dir("library-shared-duplicate-test");
    let raw = root.join("photo.CR3");
    fs::write(&raw, b"raw-bytes").unwrap();
    fs::write(crate::sidecar::sidecar_path_for_raw(&raw), b"sidecar-bytes").unwrap();
    install_test_developed_thumbnail(&raw);
    let asset = LibraryAsset::from_desktop_path(raw, 9, 1, None);

    let completion = run_duplicate_assets(vec![asset]);
    assert!(completion.result.is_ok());
    assert!(!completion.clear_clipboard);
    let duplicate = root.join("photo (1).CR3");
    assert_eq!(fs::read(&duplicate).unwrap(), b"raw-bytes");
    assert_eq!(
        fs::read(crate::sidecar::sidecar_path_for_raw(&duplicate)).unwrap(),
        b"sidecar-bytes"
    );
    assert_test_developed_thumbnail(&duplicate);

    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn shared_clipboard_same_folder_copy_duplicates_but_cut_is_noop() {
    let root = unique_temp_dir("library-same-folder-clipboard-test");
    let raw = root.join("photo.CR3");
    fs::write(&raw, b"raw-bytes").unwrap();
    fs::write(crate::sidecar::sidecar_path_for_raw(&raw), b"sidecar-bytes").unwrap();
    install_test_developed_thumbnail(&raw);
    let asset = LibraryAsset::from_desktop_path(raw.clone(), 9, 1, None);

    let copy = run_image_paste(
        ImageClipboard {
            mode: ImageClipboardMode::Copy,
            assets: vec![asset.clone()],
        },
        LibraryTransferDestination::LocalFolder(root.clone()),
    );
    assert!(copy.result.is_ok());
    let duplicate = root.join("photo (1).CR3");
    assert_eq!(fs::read(&duplicate).unwrap(), b"raw-bytes");
    assert_eq!(
        fs::read(crate::sidecar::sidecar_path_for_raw(&duplicate)).unwrap(),
        b"sidecar-bytes"
    );
    assert_test_developed_thumbnail(&duplicate);

    let cut = run_image_paste(
        ImageClipboard {
            mode: ImageClipboardMode::Cut,
            assets: vec![asset],
        },
        LibraryTransferDestination::LocalFolder(root.clone()),
    );
    assert!(cut.result.is_ok());
    assert!(cut.clear_clipboard);
    assert!(raw.exists());
    assert!(!root.join("photo (2).CR3").exists());

    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn partial_cut_keeps_only_failed_assets_on_clipboard() {
    let root = unique_temp_dir("library-partial-cut-test");
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&destination).unwrap();
    let moved = source.join("moved.CR3");
    let missing = source.join("missing.CR3");
    fs::write(&moved, b"raw").unwrap();

    let completion = run_image_paste(
        ImageClipboard {
            mode: ImageClipboardMode::Cut,
            assets: vec![
                LibraryAsset::from_desktop_path(moved.clone(), 3, 1, None),
                LibraryAsset::from_desktop_path(missing.clone(), 0, 1, None),
            ],
        },
        LibraryTransferDestination::LocalFolder(destination.clone()),
    );

    assert!(completion.result.is_err());
    assert!(!completion.clear_clipboard);
    assert!(!moved.exists());
    assert_eq!(fs::read(destination.join("moved.CR3")).unwrap(), b"raw");
    let remaining = completion.remaining_clipboard.unwrap();
    assert_eq!(remaining.assets.len(), 1);
    assert_eq!(remaining.assets[0].desktop_path(), Some(missing.as_path()));
    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn broken_developed_thumbnail_cache_does_not_block_raw_bundle_operations() {
    let root = unique_temp_dir("library-broken-developed-thumbnail-test");
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&destination).unwrap();

    let copied_source = source.join("copy.CR3");
    fs::write(&copied_source, b"copy-raw").unwrap();
    fs::write(
        crate::sidecar::sidecar_path_for_raw(&copied_source),
        b"copy-sidecar",
    )
    .unwrap();
    install_test_developed_thumbnail(&copied_source);
    fs::write(
        crate::sidecar::developed_thumbnail_path_for_raw(&copied_source),
        b"not-a-jpeg",
    )
    .unwrap();

    let copied = copy_raw_bundle_to_folder(
        &copied_source,
        copied_source.file_name().unwrap(),
        &destination,
    )
    .unwrap();
    assert_eq!(fs::read(&copied).unwrap(), b"copy-raw");
    assert_eq!(
        fs::read(crate::sidecar::sidecar_path_for_raw(&copied)).unwrap(),
        b"copy-sidecar"
    );
    assert!(crate::sidecar::load_developed_thumbnail_cache(&copied, 512)
        .unwrap()
        .is_none());

    let rename_source = source.join("before.NEF");
    fs::write(&rename_source, b"rename-raw").unwrap();
    fs::write(
        crate::sidecar::sidecar_path_for_raw(&rename_source),
        b"rename-sidecar",
    )
    .unwrap();
    install_test_developed_thumbnail(&rename_source);
    fs::write(
        crate::sidecar::developed_thumbnail_path_for_raw(&rename_source),
        b"not-a-jpeg",
    )
    .unwrap();

    let renamed = rename_raw_bundle(&rename_source, "after.NEF").unwrap();
    assert_eq!(fs::read(&renamed).unwrap(), b"rename-raw");
    assert_eq!(
        fs::read(crate::sidecar::sidecar_path_for_raw(&renamed)).unwrap(),
        b"rename-sidecar"
    );
    assert!(!rename_source.exists());

    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn rename_raw_keeps_matching_sidecar() {
    let root = unique_temp_dir("library-rename-test");
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
    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn folder_names_are_single_safe_path_components() {
    assert!(validate_folder_name("Photos 2026").is_ok());
    for invalid in ["", " ", ".", "..", "../outside", "nested/folder", "/tmp"] {
        assert!(
            validate_folder_name(invalid).is_err(),
            "accepted {invalid:?}"
        );
    }
}

#[cfg(not(target_os = "android"))]
#[test]
fn folder_operations_stay_inside_library_and_protect_root() {
    let base = unique_temp_dir("library-folder-boundary-test");
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

#[cfg(not(target_os = "android"))]
#[test]
fn recursive_folder_copy_never_overwrites_and_rejects_symlinks() {
    let root = unique_temp_dir("library-folder-copy-test");
    let source = root.join("source");
    let destination = root.join("destination");
    fs::create_dir_all(source.join("nested")).unwrap();
    fs::write(source.join("nested/photo.dng"), b"raw").unwrap();
    copy_directory_create_new(&source, &destination).unwrap();
    assert_eq!(
        fs::read(destination.join("nested/photo.dng")).unwrap(),
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

#[cfg(not(target_os = "android"))]
#[test]
fn dropped_raw_import_preserves_name_and_never_overwrites() {
    let root = unique_temp_dir("library-import-test");
    let source_folder = root.join("source");
    let library_folder = root.join("library");
    fs::create_dir_all(&source_folder).unwrap();
    fs::create_dir_all(&library_folder).unwrap();
    let source = source_folder.join("photo.CR3");
    fs::write(&source, b"new-raw").unwrap();

    let first = match import_raw_into_folder(&source, &library_folder).unwrap() {
        RawImportOutcome::Imported(path) => path,
        RawImportOutcome::AlreadyPresent => panic!("external source was not imported"),
    };
    fs::write(&source, b"newer-raw").unwrap();
    let second = match import_raw_into_folder(&source, &library_folder).unwrap() {
        RawImportOutcome::Imported(path) => path,
        RawImportOutcome::AlreadyPresent => panic!("changed source was not imported"),
    };
    assert_eq!(first.file_name().unwrap(), "photo.CR3");
    assert_eq!(second.file_name().unwrap(), "photo (1).CR3");
    assert_eq!(fs::read(first).unwrap(), b"new-raw");
    assert_eq!(fs::read(second).unwrap(), b"newer-raw");
    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn folder_scan_only_includes_direct_raw_children() {
    let root = unique_temp_dir("library-scan-test");
    let nested = root.join("nested");
    fs::create_dir_all(&nested).unwrap();
    fs::write(root.join("one.DNG"), b"raw").unwrap();
    fs::write(nested.join("two.nef"), b"raw").unwrap();
    fs::write(root.join("ignore.jpg"), b"jpeg").unwrap();

    let (assets, warnings, truncated) = scan_folder(&root, || false).unwrap().unwrap();
    let names = assets
        .iter()
        .map(|asset| asset.display_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(warnings, 0);
    assert!(!truncated);
    assert_eq!(names, vec!["one.DNG"]);
    fs::remove_dir_all(root).unwrap();
}

#[cfg(not(target_os = "android"))]
#[test]
fn folder_scan_retains_newest_files_after_limit() {
    let root = unique_temp_dir("library-limit-test");
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

    let (assets, warnings, truncated) =
        scan_folder_with_limit(&root, 3, || false).unwrap().unwrap();
    let names = assets
        .iter()
        .map(|asset| asset.display_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["newest.dng", "newer.dng", "middle.dng"]);
    assert_eq!(warnings, 0);
    assert!(truncated);
    fs::remove_dir_all(root).unwrap();
}
