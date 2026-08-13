# TIFF contract

AuRaw classifies `.tif` and `.tiff` files as sensor containers or rendered
rasters before decoding.

## Import

- Accept Classic TIFF and BigTIFF in either byte order.
- Reject cyclic, oversized, inconsistent, or out-of-bounds IFD data.
- Route CFA/LinearRaw images through the RAW path; calibration metadata alone
  does not prove that a file contains a sensor mosaic.
- Bound dimensions, allocations, and ICC tag 34675 before decoding.
- Convert untagged integer RGB from sRGB to scene-linear Rec.2020 D65.
- Treat untagged float RGB as scene-linear Rec.2020 D65 and preserve HDR and
  negative values.
- Skip demosaic, sensor white balance, and RAW denoise for rendered rasters.

## Export

- 8/16-bit TIFF is ICC-managed RGB; 32-bit float TIFF is linear Rec.2020 D65.
- Stream full-quality tiles into roughly 1 MiB Classic TIFF strips.
- Check offsets, byte counts, raster coverage, and the 4 GiB address limit.

Tests cover routing, malformed input, ICC handling, precision, planar RGB,
thumbnail color, and strip layout.
