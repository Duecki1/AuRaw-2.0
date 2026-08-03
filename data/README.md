# White-balance preset data

`wb_presets.json` is a compact, zero-fine-tuning subset of darktable's camera
white-balance preset database. It retains the camera maker/model, preset name,
and channel coefficients needed by AuRaw's preset chooser. The source database
is `data/wb_presets.json` in the darktable project.

The data is distributed under the GNU General Public License, version 3 or
later, consistently with both darktable and AuRaw. Its original preset data was
developed by the darktable and UFRaw contributors; see darktable's
`src/common/wb_presets.c` for attribution and format documentation.
