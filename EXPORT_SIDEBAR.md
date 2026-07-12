# Sidebar and export changes

## Sidebar tabs

The Develop sidebar now has four persistent tabs:

1. Adjustments
2. Masks
3. Inpainting
4. Export

Masks and Inpainting intentionally contain placeholders only. Their tab state
is already part of the application UI model, so real tools can replace the
placeholder panels without another layout migration.

## Export sizing

The Export tab supports these aspect-ratio-preserving modes:

- Original size
- Long edge
- Short edge
- Width
- Height
- Percentage

The output dimensions shown by the UI are calculated by the same
`ExportSettings::output_dimensions` method used by the export worker. Requested
sizes do not enlarge the image unless **Allow upscaling** is enabled.

When resizing is requested, AuRaw area-resamples the RAW mosaic while averaging
only samples belonging to the same CFA plane. The resampled mosaic is then sent
through the normal high-quality demosaic and Develop pipeline at the exact
requested output dimensions.

## Metadata

**Keep metadata** is enabled by default. PNG exports include:

- Camera make and model when available
- Original source filename when available
- Original and exported pixel dimensions
- AuRaw software identification
- EXIF orientation normalized to 1, since AuRaw physically orients decoded RAWs

Metadata is written through PNG eXIf and UTF-8 iTXt chunks. Disabling the option
omits those chunks while retaining the required sRGB color-space declaration.
