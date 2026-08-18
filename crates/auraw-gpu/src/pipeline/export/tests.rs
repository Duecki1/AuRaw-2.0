use super::{
    bounded_tile_spec, build_exif_payload, build_lanczos_contributions, built_in_srgb_icc,
    encode_jpeg_rgb, encode_srgb_row, encode_srgb_row_with_format, export_to_destination,
    publish_completed_export, resolved_export_tile_spec, stitch_linear_tile_into_band,
    tiff_strip_layout, tile_mask_source_region, validate_export_dimensions, ExportFormat,
    ExportMetadata, ExportResizeMode, ExportRowFormat, ExportSettings, GeometryResampler,
    JpegEncodeRequest, LinearLightResizer,
    EXPORT_TILE_HALO, MAX_EXPORT_EDGE, TIFF_TARGET_STRIP_BYTES,
};
use crate::pipeline::{
    ExportTile, ExposureParams, GeometryTransform, IccOutputTransform, MaskStack, TileSpec,
};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn export_format_extensions_preserve_existing_names_and_aliases() {
    assert_eq!(ExportFormat::Png.extension(), "png");
    assert_eq!(ExportFormat::Jpeg.extension(), "jpg");
    assert_eq!(ExportFormat::Tiff.extension(), "tif");
    assert!(ExportFormat::Png.matches_extension("PNG"));
    assert!(ExportFormat::Jpeg.matches_extension("jpeg"));
    assert!(ExportFormat::Tiff.matches_extension("TIFF"));
    assert!(!ExportFormat::Png.matches_extension("jpg"));
}

#[test]
fn resize_modes_preserve_aspect_ratio() {
    let base = ExportSettings::default();
    let cases = [
        (ExportResizeMode::LongEdge, 3000, (3000, 2000)),
        (ExportResizeMode::ShortEdge, 1000, (1500, 1000)),
        (ExportResizeMode::Width, 1200, (1200, 800)),
        (ExportResizeMode::Height, 800, (1200, 800)),
    ];
    for (resize_mode, edge_or_dimension, expected) in cases {
        let settings = ExportSettings {
            resize_mode,
            edge_or_dimension,
            ..base.clone()
        };
        assert_eq!(settings.output_dimensions(6000, 4000), expected);
    }
}

#[test]
fn resizing_does_not_enlarge_by_default() {
    let settings = ExportSettings {
        resize_mode: ExportResizeMode::LongEdge,
        edge_or_dimension: 12000,
        ..ExportSettings::default()
    };
    assert_eq!(settings.output_dimensions(6000, 4000), (6000, 4000));
}

#[test]
fn export_mask_region_covers_the_padded_tile_and_clamps_to_source() {
    let region =
        tile_mask_source_region(&MaskStack::default(), -256, 744, 1536, 1536, 6000, 4000);
    assert_eq!(region, [0, 742, 1282, 1540]);
}

#[test]
fn exif_payload_contains_source_camera_lens_and_exposure_metadata() {
    let metadata = ExportMetadata {
        source_file_name: Some("IMG_0042.CR3".to_owned()),
        camera_make: "CameraCo".to_owned(),
        camera_model: "Model X".to_owned(),
        lens_make: "LensCo".to_owned(),
        lens_model: "Prime 50".to_owned(),
        focal_length: 50.0,
        aperture: 2.8,
        iso_speed: 640.0,
        shutter_seconds: 1.0 / 125.0,
        description: "Studio portrait".to_owned(),
        artist: "Photographer".to_owned(),
        source_width: 6000,
        source_height: 4000,
        ..ExportMetadata::default()
    };
    let exif = build_exif_payload(&metadata, 3000, 2000);
    assert_eq!(&exif[..4], &[b'I', b'I', 42, 0]);
    for expected in [
        b"CameraCo\0".as_slice(),
        b"Model X\0".as_slice(),
        b"LensCo\0".as_slice(),
        b"Prime 50\0".as_slice(),
        b"IMG_0042.CR3".as_slice(),
        b"Studio portrait".as_slice(),
        b"Photographer\0".as_slice(),
    ] {
        assert!(exif
            .windows(expected.len())
            .any(|window| window == expected));
    }

    let read_u16 = |offset: usize| u16::from_le_bytes([exif[offset], exif[offset + 1]]);
    let read_u32 = |offset: usize| {
        u32::from_le_bytes([
            exif[offset],
            exif[offset + 1],
            exif[offset + 2],
            exif[offset + 3],
        ])
    };
    let ifd0_offset = read_u32(4) as usize;
    let ifd0_count = read_u16(ifd0_offset) as usize;
    let mut exif_ifd_offset = None;
    let mut ifd0_tags = Vec::new();
    for index in 0..ifd0_count {
        let entry = ifd0_offset + 2 + index * 12;
        let tag = read_u16(entry);
        ifd0_tags.push(tag);
        if tag == 0x8769 {
            exif_ifd_offset = Some(read_u32(entry + 8) as usize);
        }
    }
    assert!(ifd0_tags.contains(&0x010e));
    assert!(ifd0_tags.contains(&0x010f));
    assert!(ifd0_tags.contains(&0x0110));
    assert!(ifd0_tags.contains(&0x013b));

    let exif_ifd_offset = exif_ifd_offset.expect("ExifIFD pointer");
    let exif_count = read_u16(exif_ifd_offset) as usize;
    let exif_tags = (0..exif_count)
        .map(|index| read_u16(exif_ifd_offset + 2 + index * 12))
        .collect::<Vec<_>>();
    for tag in [
        0x829a, 0x829d, 0x8827, 0x920a, 0x9286, 0xa002, 0xa003, 0xa433, 0xa434,
    ] {
        assert!(exif_tags.contains(&tag), "missing EXIF tag {tag:#06x}");
    }
}

#[test]
fn jpeg_rows_omit_png_alpha_bytes() {
    let transform = crate::pipeline::IccOutputTransform::srgb();
    let rgba = encode_srgb_row(&[0.18, 0.18, 0.18], &transform).unwrap();
    let rgb =
        encode_srgb_row_with_format(&[0.18, 0.18, 0.18], &transform, ExportRowFormat::Rgb8)
            .unwrap();
    assert_eq!(rgba.len(), 4);
    assert_eq!(rgb.len(), 3);
    assert_eq!(&rgba[..3], &rgb);
}

#[test]
fn fast_jpeg_encoder_writes_decodable_pixels_exif_and_icc() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("auraw-jpeg-encoder-{}-{nonce}", std::process::id()));
    std::fs::create_dir_all(&directory).unwrap();
    let destination = directory.join("photo.jpg");
    let width = 16u32;
    let height = 8u32;
    let rgb = (0..width as usize * height as usize * 3)
        .map(|index| index.wrapping_mul(37) as u8)
        .collect::<Vec<_>>();
    let metadata = ExportMetadata {
        camera_make: "CameraCo".to_owned(),
        camera_model: "Fast JPEG".to_owned(),
        ..ExportMetadata::default()
    };
    let icc = built_in_srgb_icc();

    encode_jpeg_rgb(JpegEncodeRequest {
        rgb: &rgb,
        output_path: &destination,
        width,
        height,
        quality: 90,
        keep_metadata: true,
        metadata: &metadata,
        icc_profile: Some(&icc),
    })
    .unwrap();

    let encoded = std::fs::read(&destination).unwrap();
    assert_eq!(&encoded[..2], &[0xff, 0xd8]);
    assert!(encoded
        .windows(b"Exif\0\0".len())
        .any(|window| window == b"Exif\0\0"));
    assert!(encoded
        .windows(b"ICC_PROFILE\0".len())
        .any(|window| window == b"ICC_PROFILE\0"));
    let decoded = image::load_from_memory_with_format(&encoded, image::ImageFormat::Jpeg)
        .expect("fast JPEG should decode");
    assert_eq!((decoded.width(), decoded.height()), (width, height));

    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn tiff_strip_layout_is_chunked_and_covers_the_raster_exactly() {
    let width = 4_000u32;
    let height = 3_000u32;
    let (rows_per_strip, byte_counts) = tiff_strip_layout(width, height, 16).unwrap();
    assert!(rows_per_strip > 0);
    assert!(byte_counts.len() > 1);
    let row_bytes = u64::from(width) * 6;
    assert!(u64::from(byte_counts[0]) <= TIFF_TARGET_STRIP_BYTES);
    assert_eq!(
        byte_counts
            .iter()
            .map(|value| u64::from(*value))
            .sum::<u64>(),
        u64::from(width) * u64::from(height) * 6
    );
    assert!(byte_counts
        .iter()
        .all(|count| u64::from(*count) % row_bytes == 0));
}

#[test]
fn tiff_strip_layout_uses_one_row_when_a_row_exceeds_the_target() {
    let width = 100_000u32;
    let height = 3u32;
    let (rows_per_strip, byte_counts) = tiff_strip_layout(width, height, 32).unwrap();
    assert_eq!(rows_per_strip, 1);
    assert_eq!(byte_counts, vec![1_200_000; 3]);
}

#[test]
fn tile_rows_land_at_their_band_offset() {
    let mut band = vec![0.0f32; 4 * 3];
    let tile = ExportTile {
        core_x: 1,
        core_y: 2,
        core_width: 2,
        core_height: 1,
        local_core_x: 48,
        local_core_y: 48,
        padded_width: 100,
        padded_height: 100,
        global_origin_x: -47,
        global_origin_y: -46,
    };
    stitch_linear_tile_into_band(&mut band, 4, 2, tile, &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0])
        .unwrap();
    assert_eq!(&band[3..9], &[1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
}

#[test]
fn resize_kernels_are_bounded_and_normalized() {
    let kernels = build_lanczos_contributions(32_768, 1).unwrap();
    assert_eq!(kernels.len(), 1);
    assert!(kernels[0].len() <= 32_768);
    let sum: f32 = kernels[0].iter().map(|sample| sample.weight).sum();
    assert!((sum - 1.0).abs() < 1e-5);
}

#[test]
fn identity_resize_uses_one_exact_sample_per_pixel() {
    let kernels = build_lanczos_contributions(8, 8).unwrap();
    for (index, kernel) in kernels.iter().enumerate() {
        assert_eq!(kernel.len(), 1);
        assert_eq!(kernel[0].index, index as u32);
        assert_eq!(kernel[0].weight, 1.0);
    }
}

#[test]
fn geometry_resampler_identity_is_exact_in_linear_space() {
    let source = (0..4 * 3 * 3)
        .map(|index| index as f32 / 37.0)
        .collect::<Vec<_>>();
    let resampler =
        GeometryResampler::new(&source, 4, 3, GeometryTransform::default(), 4, 3).unwrap();
    let mut output = Vec::new();
    for y in 0..3 {
        output.extend_from_slice(&resampler.output_row(y).unwrap());
    }
    assert_eq!(output, source);
}

#[test]
fn geometry_resampler_quarter_turn_preserves_exact_pixels() {
    let source = [
        1.0, 1.0, 1.0, 2.0, 2.0, 2.0, 3.0, 3.0, 3.0, 4.0, 4.0, 4.0, 5.0, 5.0, 5.0, 6.0, 6.0,
        6.0,
    ];
    let geometry = GeometryTransform {
        quarter_turns: 1,
        ..Default::default()
    };
    let resampler = GeometryResampler::new(&source, 3, 2, geometry, 2, 3).unwrap();
    let expected = [
        4.0, 4.0, 4.0, 1.0, 1.0, 1.0, 5.0, 5.0, 5.0, 2.0, 2.0, 2.0, 6.0, 6.0, 6.0, 3.0, 3.0,
        3.0,
    ];
    let mut output = Vec::new();
    for y in 0..3 {
        output.extend_from_slice(&resampler.output_row(y).unwrap());
    }
    assert_eq!(output, expected);
}

#[test]
fn geometry_downsample_accumulates_linear_values_before_encoding() {
    let source = [0.0, 0.0, 0.0, 1.0, 1.0, 1.0];
    let resampler =
        GeometryResampler::new(&source, 2, 1, GeometryTransform::default(), 1, 1).unwrap();
    let row = resampler.output_row(0).unwrap();
    for value in &row {
        assert!((*value - 0.5).abs() < 1e-5);
    }
    let encoded =
        encode_srgb_row_with_format(&row, &IccOutputTransform::srgb(), ExportRowFormat::Rgb8)
            .unwrap();
    assert!(encoded.iter().all(|value| *value > 170));
}

#[test]
fn export_dimension_limits_reject_oversized_images() {
    assert!(validate_export_dimensions(MAX_EXPORT_EDGE + 1, 1).is_err());
    assert!(validate_export_dimensions(MAX_EXPORT_EDGE, MAX_EXPORT_EDGE).is_err());
}

#[test]
fn wide_sources_reduce_band_height_to_stay_within_budget() {
    let requested = crate::pipeline::TileSpec {
        core_edge: if cfg!(target_os = "android") {
            768
        } else {
            1024
        },
        halo: EXPORT_TILE_HALO,
    };
    let bounded = bounded_tile_spec(requested, MAX_EXPORT_EDGE).unwrap();
    assert!(bounded.core_edge <= requested.core_edge);
    assert!(bounded.core_edge >= 64);
}

#[test]
fn resolving_export_halo_never_enlarges_the_requested_tile() {
    let requested = TileSpec {
        core_edge: 768,
        halo: EXPORT_TILE_HALO,
    };
    let resolved = resolved_export_tile_spec(
        requested,
        &ExposureParams::scene_referred_default(),
        &MaskStack::default(),
        8_640,
    )
    .unwrap();
    assert_eq!(resolved.core_edge, requested.core_edge);
    assert!(resolved.halo <= requested.halo);
}

#[test]
fn vertical_resize_streams_extreme_upscales_without_retaining_rows() {
    let transform = crate::pipeline::IccOutputTransform::srgb();
    let mut output = Vec::new();
    let mut resizer = LinearLightResizer::new(1, 1, 1, 128).unwrap();
    resizer
        .push_source_row(0, &[0.18, 0.18, 0.18], Some(&transform), &mut output)
        .unwrap();
    assert!(resizer.pending_rows.iter().all(Option::is_none));
    resizer.finish(Some(&transform), &mut output).unwrap();
    assert_eq!(output.len(), 128 * 4);
}

#[test]
fn vertical_resize_streams_extreme_downscales_with_one_active_row() {
    let transform = crate::pipeline::IccOutputTransform::srgb();
    let mut output = Vec::new();
    let mut resizer = LinearLightResizer::new(1, 128, 1, 1).unwrap();
    for source_y in 0..128 {
        resizer
            .push_source_row(source_y, &[0.18, 0.18, 0.18], Some(&transform), &mut output)
            .unwrap();
        assert!(
            resizer
                .pending_rows
                .iter()
                .filter(|row| row.is_some())
                .count()
                <= 1
        );
    }
    resizer.finish(Some(&transform), &mut output).unwrap();
    assert_eq!(output.len(), 4);
}

#[test]
fn srgb_encoding_outputs_opaque_rgba_and_rejects_non_finite_values() {
    let transform = crate::pipeline::IccOutputTransform::srgb();
    let encoded = encode_srgb_row(&[0.0, 0.18, 1.0], &transform).unwrap();
    assert_eq!(encoded.len(), 4);
    assert_eq!(encoded[3], 255);
    assert!(encode_srgb_row(&[f32::NAN, 0.0, 0.0], &transform).is_err());
}

#[test]
fn cancelled_export_removes_temporary_output_before_publication() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "auraw-export-cancel-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let destination = directory.join("photo.png");
    let cancellation = AtomicBool::new(false);

    let result = export_to_destination(&destination, &cancellation, |temporary| {
        std::fs::write(temporary, b"complete but not published")?;
        cancellation.store(true, Ordering::Release);
        Ok(())
    });

    assert!(result.is_err());
    assert!(!destination.exists());
    assert!(std::fs::read_dir(&directory).unwrap().next().is_none());
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn failed_export_publish_preserves_existing_destination() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "auraw-export-publish-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir_all(&directory).unwrap();
    let destination = directory.join("photo.png");
    let missing_temporary = directory.join("missing.part");
    std::fs::write(&destination, b"previous export").unwrap();

    assert!(publish_completed_export(&missing_temporary, &destination).is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), b"previous export");

    std::fs::remove_dir_all(directory).unwrap();
}
