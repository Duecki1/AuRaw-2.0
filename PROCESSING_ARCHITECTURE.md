# Proxy, cache, and tiled processing architecture

## Interactive path

1. RAW decode runs on the `auraw-decode-preview` worker.
2. The decoded sensor data is retained at full resolution in `Arc<LoadedRaw>`.
3. A CFA-aware proxy is generated with a maximum edge of 2048 px on desktop or 1280 px on Android.
4. The worker creates and submits the initial preview GPU pipeline at high quality (RGBA32F) on desktop and preview quality (RGBA16F) on Android.
5. The UI thread only registers the completed output texture with egui.

The preview pipeline keeps persistent GPU resources for three dependency stages:

- **RAW**: highlight reconstruction, demosaic, chromatic-aberration correction, and RAW chroma denoise.
- **Tone**: reduced-resolution tone guide, histogram, and global scene statistics used only by the local Highlights/Shadows/Whites/Blacks masks.
- **Output**: Develop adjustments, local basic-tone exposure shaping, darktable sigmoid, and ICC display rendering.

`affected_stage` identifies the earliest invalidated stage. RAW controls rerun RAW → Tone → Output. Ordinary Develop controls rerun Output only. One stage is submitted per event-loop iteration, and wgpu executes submissions asynchronously while upstream textures remain cached.

## Full-resolution export

Export captures the current full RAW, preview RAW, and adjustment snapshot, then runs on `auraw-tiled-export`.

1. A headless proxy pipeline computes global tone statistics for the local basic-tone masks; the darktable sigmoid coefficients come only from their explicit controls.
2. `TilePlan` splits the full image into 1024 px cores on desktop or 768 px cores on Android, each with a 48 px halo.
3. One reusable high-quality GPU pipeline receives each fixed-size padded RAW tile.
4. Tile-local tone guides are generated, while global tone statistics are copied from the proxy analysis to avoid tonal seams.
5. Only each tile core is read back.
6. Completed horizontal tile bands are streamed into the PNG encoder in scanline order, avoiding a full-resolution RGBA allocation.

Tile origin and full-image dimensions are included in the GPU uniform block so spatially dependent operations, including chromatic-aberration warping, use global image coordinates.
