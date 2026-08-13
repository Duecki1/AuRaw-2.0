# TIFF implementation contract

AuRaw treats `.tif` and `.tiff` as either **sensor containers** or **rendered
rasters**. The distinction is made before decoding so a CFA/DNG-style TIFF is
never interpreted as display RGB by the generic raster decoder.

## Import

- Classic TIFF and BigTIFF headers are accepted in either byte order.
- The bounded IFD walk rejects cycles, excessive directory counts, oversized
  directories, invalid offsets, and out-of-file tag payloads before routing.
- Strong sensor markers are CFA structure tags or CFA/LinearRaw photometric
  interpretation. Camera calibration metadata such as `ColorMatrix1` alone is
  intentionally not treated as proof of a sensor mosaic because rendered TIFF
  writers may preserve those tags.
- Raster decoding uses the Rust `image`/`tiff` stack with explicit dimension and
  allocation limits. Planar and chunky RGB are handled by that decoder stack.
- Embedded ICC tag 34675 takes precedence over fallback assumptions. ICC data is
  bounded, header-validated, and trimmed to the profile size declared by the ICC
  header before color management.
- Untagged integer TIFF is interpreted as sRGB and converted to scene-linear
  Rec.2020 D65. Untagged float TIFF is AuRaw's scene-linear Rec.2020 D65
  interchange format and preserves negative values and HDR headroom.
- Imported rendered rasters bypass CFA demosaic and sensor-only white-balance or
  AI-denoise operations.

## Export

- 8-bit and 16-bit TIFFs are ICC-managed rendered RGB.
- 32-bit float TIFF is scene-linear Rec.2020 D65 and carries a matching linear
  Rec.2020 ICC profile.
- Pixel samples are streamed from the full-quality tiled pipeline; a complete
  output image is not buffered in RAM.
- Classic TIFF strips target approximately 1 MiB of uncompressed raster data.
  This keeps strip tables small while giving downstream readers bounded decode
  units and better locality than a single image-sized strip.
- Strip offsets and byte counts are calculated with checked arithmetic, and the
  complete raster coverage is verified before the header is written.
- AuRaw's current export pixel budget keeps 32-bit RGB output below classic
  TIFF's 4 GiB address space. The writer fails explicitly if that invariant ever
  changes instead of allowing an offset wrap.

## Security and resource limits

The TIFF path is designed for untrusted files. IFD count, IFD entry count,
SubIFD fan-out, ICC size, image edge length, pixel count, and decoder allocation
are all bounded. Offset and size arithmetic uses checked operations before file
seeks or allocations.

## Validation

Keep TIFF-specific tests focused on routing, malformed-container rejection,
ICC normalization, integer/float precision, planar RGB input, thumbnail color,
and strip-layout invariants. Full workspace validation should still include
`cargo fmt`, `cargo clippy`, and the relevant `cargo test` targets on a machine
with the pinned Rust toolchain.
