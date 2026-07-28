# AuRaw 2.0 background-task queue implementation

## Architecture

The implementation adds a reusable `BackgroundTaskManager` in
`src/app/background_tasks.rs` and a task runtime/UI bridge in
`src/app/background_task_runtime.rs`.

The manager owns stable task IDs, FIFO queue order, one active task, task labels,
progress, cooperative cancellation tokens, minimized/detail-window state, global
visibility, and retained failures. `BackgroundAction` contains typed requests
captured before work starts rather than UI closures borrowing mutable application
state.

The main eframe loop starts at most one queued action at a time and automatically
advances after completion, cancellation, or failure. Existing worker channels
feed progress into the manager. The top bar renders a compact, right-aligned task
control and an anchored queue popup.

Integrated operations:

- Single-image export
- Library batch export as one queue task
- Lens correction and preview-proxy preparation
- Subject-mask model download and inference
- Object-mask model download and inference
- Inpainting model download and inference
- Library AI-mask refresh

## Develop isolation for desktop batch export

Desktop library batch export no longer opens each source RAW through the main
application document loader. It owns a separate worker that captures the batch
request and then, for each source in FIFO order:

1. Loads the source sidecar.
2. Decodes the RAW through the shared LibRaw decode gate.
3. Applies the saved camera profile, geometry, lens correction, masks, and
   inpainting state.
4. Reconstructs missing canonical range-mask sources when required.
5. Runs the existing tiled PNG/JPEG/TIFF export worker.
6. Reports per-image and per-tile progress through a dedicated batch receiver.

The worker never writes `current_path`, `loaded_raw`, `preview_raw`, `active_tab`,
or the Develop preview pipeline. Completing a JPEG therefore cannot switch the
user back to Library or replace the RAW currently open in Develop. Batch tile
progress is stored separately from single-image Develop export progress.

Opening another RAW or changing the current camera profile is no longer blocked
merely because an export receiver or Android publication state exists. A new RAW
open can still wait for a currently executing native LibRaw decode because LibRaw
access is serialized by the existing shared decode gate; the UI remains
responsive and the batch worker does not consume the opened document.

## AI task visibility

The global top-bar task control shows AI work only while model files are being
downloaded:

- `Downloading subject-mask model`
- `Downloading object-mask model`
- `Downloading inpainting model`

When all required model files already exist, interactive subject-mask,
object-mask, and inpainting inference starts immediately as a non-blocking task.
It does not wait for an export, lens correction, or another FIFO background task
to finish. If a model must be downloaded first, the download remains a visible
FIFO task; as soon as the worker reports that inference is beginning, it hides
from the global control and releases the FIFO execution slot so the next queued
background action can start while local inference continues concurrently.

The detached inference task retains the same stable task ID, cancellation token,
detail-window state, and document/generation safeguards. The operation-specific
progress window remains cancellable and can be minimized. Library AI-mask refresh
continues to use its existing serialized library workflow and is hidden from the
global control except while a missing model is being downloaded.

## Cancellation and stale-result safeguards

- Queued cancellation removes the stable task and typed request immediately.
- Running cancellation sets an atomic token and changes the shared status to
  `Cancelling…`.
- Export workers check cancellation between safe phases, tiles, and rows. Desktop
  batch cancellation does not start another source. If the current image was
  already published when cancellation arrived, it is counted as completed; a
  cooperative cancellation result is not recorded as a failure.
- Model downloads check cancellation while transferring and before publishing a
  temporary model file.
- Lens tasks carry document and lens-generation identities. New requests coalesce
  obsolete queued requests for the same document and cancel an older active
  generation.
- Subject, object, and inpainting tasks carry document/edit-generation identities.
  Results are validated before application and stale or cancelled results are
  discarded.
- Worker disconnection terminates the task instead of leaving a receiver attached
  indefinitely, allowing the next FIFO item to start.
- Native ONNX and Lensfun calls that cannot be interrupted internally stop at the
  next safe phase boundary; the UI communicates this during cancellation.

## Changed files

- `src/ai_masks.rs`
- `src/inpainting.rs`
- `src/pipeline/export.rs`
- `src/app.rs`
- `src/app/background_tasks.rs` (new)
- `src/app/background_task_runtime.rs` (new)
- `src/app/edit_history.rs`
- `src/app/eframe_impl.rs`
- `src/app/inpainting.rs`
- `src/app/library_adjustments.rs`
- `src/app/lifecycle.rs`
- `src/app/masks_ai.rs`
- `src/app/processing_export.rs`
- `src/ui/library.rs`
- `src/ui/top_bar.rs`
- `tests/test_background_export_isolation.py` (new)

`BACKGROUND_TASKS_IMPLEMENTATION.patch` contains the complete source diff against
the uploaded AuRaw baseline. The separately supplied follow-up patch contains
only the Develop-isolation and AI-download-visibility changes relative to the
previous background-queue archive.

## Validation performed in this environment

Passed:

```text
pytest -q
389 passed

python scripts/check-source-tree.py
source tree contains only connected modules and tracked shader sources

python scripts/check-source-connectivity.py
source tree contains only connected modules and tracked shader sources
```

The non-UI manager includes Rust unit tests for FIFO ordering, queue advancement,
queued removal, cooperative running cancellation, waiting count, stable-ID
progress updates, stale updates, lens-task coalescing, exclusion of hidden AI
inference tasks from the global control, immediate non-blocking inference, FIFO
slot release after model download, and cancellation of detached inference.

Not executed here:

```text
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features
```

This container has no `cargo`, `rustc`, `rustfmt`, or `clippy-driver` executable.
The Rust build therefore could not start. No native-library failure involving
LibRaw, Lensfun, or ONNX Runtime was reached.

## Remaining limitations

- Android library batch export still uses the platform document-opening and
  publication workflow and can return to Library between items. The desktop
  isolation described above is not duplicated on Android.
- Library AI-mask refresh still uses AuRaw's existing document loader and can
  temporarily replace the loaded document while refreshing library items. Its
  generation/inference phases are hidden from the global top bar as requested.
- Interactive AI inference now runs concurrently with exports and other queued
  work rather than pausing those workers. Heavy concurrent CPU/GPU activity can
  still contend for hardware resources, but inference no longer waits for the
  FIFO queue. First-use model downloads remain serialized FIFO tasks.
- Batch-export progress now reserves 10% of each current image for encoding,
  metadata writing, publication, and final rename. A fully rendered tile set no
  longer advances the batch to 100%; completion reaches 100% only after every
  image reports `ItemFinished`. The UI reports `Finalizing <filename>` during
  the reserved final phase.
- Canonical AI-mask source capture still uses the existing UI-thread GPU readback
  preflight before the queued download/inference phase begins.
- LibRaw decode is serialized. Opening a RAW can wait for a batch RAW decode that
  is already inside LibRaw, although it no longer waits for JPEG rendering or
  publication and no batch result mutates Develop state.
- Lensfun and ONNX native calls are cooperatively cancellable only between safe
  phases, not from inside the native call.
- Rust formatting and compilation must be verified with the pinned Rust 1.92.0
  toolchain on a machine with the project's native build prerequisites.

## Android batch-export dialog sequencing fix

Android batch export now enqueues with its task-details window closed. The FIFO runner opens the shared details state only after the batch task has actually started and its operation-specific `Exporting images` window exists. This prevents the export-settings confirmation window and a generic `Waiting for earlier background work…` task window from being painted together during the enqueue frame. The queued phase is now `Queued for batch export…`; when execution begins it changes to `Preparing batch export…`.


## Android nested batch-export task fix

The remaining Android double-dialog was not an enqueue-frame paint race. The
batch task opened each selected RAW and then called the normal single-image
`start_export()` path. That created a second `SingleExport` task behind the
already-running `LibraryBatchExport` task. Because the batch task was waiting
for that child export before it could finish, the child could never acquire the
FIFO slot and its generic `Exporting image` window remained on top with
`Waiting for earlier background work…`.

Android batch items now capture the export request and start the tiled export
worker directly under the existing batch task ID. No nested global task is
created. The operation therefore has one `Exporting images` dialog, one
cancellation token, and one FIFO owner for the complete batch.

## Concurrent RAW-open/export GPU-residency fix

Opening a RAW with range or promptable-object masks previously allocated the
normal Develop preview pipeline and then a second full preview pipeline solely
to reconstruct the canonical neutral mask source. A tiled export of the same
RAW could already hold enough resident GPU resources that this temporary second
pipeline crossed the process budget. The RAW decode completed, but Develop then
failed with `range-mask source setup failed: GPU pipelines already reserve ...`.

The open worker now renders the canonical neutral mask source through the newly
created Develop preview pipeline itself. It uploads the composed inpainting
layer once, renders and reads the neutral sRGB source with zero local masks,
installs the missing range sources, uploads the actual masks, restores the
configured desktop display transform, and renders the edited preview. No second
GPU pipeline or second mask atlas is allocated. The same path is compiled for
Android and desktop.

A source regression test verifies that the canonical-source block does not
construct another `RawGpuPipeline`, uses `preview_raw`, and applies the desktop
display transform only after the neutral canonical readback.

## Android foreground-task mode

Android now treats every queued, running, cancelling, or unacknowledged failed
long-running task as foreground-modal work. A full-content input shield is
installed above Library/Develop while a task exists, and operation progress
windows are promoted to egui's foreground order so their progress and Cancel or
Dismiss controls remain interactive. The system Back action and edit/sidecar
shortcuts are ignored during the modal interval.

Android RAW-picker entry points also reject programmatic or same-frame open
requests while foreground work is active. This is a second line of defense in
case an input was already in flight when the task began. As a result, tapping
the same RAW while a library export owns the mobile GPU cannot allocate a second
preview pipeline or abort the export. The user remains on the current screen
until the operation finishes or is cancelled.

All `Minimize` controls are now compiled only for non-Android targets. Desktop
retains the global queue, background operation model, and minimizable detail
windows. Android retains FIFO execution and cancellation but does not allow the
progress window to be hidden or the rest of the application to be used during
active work. Interactive AI inference still bypasses the FIFO slot as previously
requested, but its Android progress window is modal until inference completes.

## Validation for the Android foreground-task follow-up

Executed in this environment:

```text
python scripts/check-source-tree.py
python scripts/check-source-connectivity.py
pytest -q
```

Results:

```text
source tree contains only connected modules and tracked shader sources
source tree contains only connected modules and tracked shader sources
393 passed
```

The Rust toolchain is still absent from this container, so `cargo fmt --check`,
`cargo check`, `cargo test`, and Clippy could not be executed. This is an
environment limitation; no Rust or native-library build was started.

## Desktop top-bar progress alignment follow-up

The desktop global task control now expands its own compact horizontal row to
match the top panel's available height, then relies on egui's horizontal
cross-axis centering. This centers the task name, progress bar/spinner, and `+N`
badge vertically without using `Ui::horizontal_centered`, which would reserve
the full remaining width and interfere with the wrapped navigation controls.
Android layout is unchanged.

A source regression test verifies the desktop-only height expansion and guards
against replacing it with a full-width centered row.

## Android background execution research

No Android service, notification, WorkManager, UIDT, or process-lifecycle code
was implemented. A detailed feasibility and architecture study is included in
`ANDROID_BACKGROUND_EXECUTION_RESEARCH.md`.
