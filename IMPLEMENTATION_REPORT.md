# AuRaw 2.0 background-task implementation — final review

## Scope

This tree contains the complete global task-queue implementation and all later
follow-up fixes. The final review compared the complete patch with the uploaded
AuRaw baseline, traced task ownership and cancellation through the desktop and
Android paths, checked the task-manager invariants, searched the modified source
for duplicate helpers and dead wrappers, and reviewed the Java notification
bridge and Android lifecycle integration.

## Architecture retained

- `src/app/background_tasks.rs` owns stable task IDs, FIFO order, one serialized
  task slot, detached latency-sensitive AI inference, progress, cancellation,
  detail-window state, global visibility, and acknowledged failures.
- `src/app/background_task_runtime.rs` connects typed `BackgroundAction` requests
  to existing workers and renders the top-bar control, queue popup, and shared
  task-detail windows.
- Desktop batch export captures and decodes each source independently of the
  Develop document.
- Android long operations are foreground-modal inside the Activity. The Android
  notification mirrors progress but is not a foreground service; AuRaw must
  remain open.
- Subject/object-mask and inpainting inference bypass the serialized slot when
  their models are already available. Only model downloads are globally visible.

## Problems found and fixed during the final review

### Queue and global UI

- Consolidated duplicated task construction into one `insert_task` helper.
- Consolidated snapshot ordering for full and globally visible task lists.
- Fixed detached visible work disappearing from the global task list after it
  released the FIFO slot.
- Fixed `+N` so it counts only tasks waiting after the item displayed in the
  compact control.
- Removed unused task-runtime wrapper methods.
- Routed an impossible/missing queued action through the normal failure helper so
  the UI is repainted and the retained error is visible.
- Unknown `completed/total` progress now renders as indeterminate instead of a
  misleading static `0/0` bar.

### Export and batch export

- Removed duplicated PNG/JPEG/TIFF dispatch from the runtime and centralized it
  in `spawn_export_request`.
- Consolidated platform-independent batch finish, status, progress, tile, and
  cancellation helpers.
- Ensured Android batch export does not create a nested single-export task.
- Ensured Android cancellation while a batch RAW is still opening cannot start
  the export afterward.
- Routed synchronous Android document-open failures back to the batch owner so
  the batch cannot wait forever for a receiver that was never created.
- Released the temporary Android Develop preview pipeline before starting tiled
  export, avoiding simultaneous mobile preview/export residency.
- Reserved final progress for encoding, metadata, publication, and rename; tile
  completion alone no longer reports 100%.
- Added a final cancellation check before publication and temporary-output
  cleanup where the export path permits it.

### Lens, masks, and inpainting

- Consolidated desktop/Android lens queueing and worker preparation while
  preserving Android cached-correction reuse.
- Preserved stale document/generation checks before applying lens, mask, or
  inpainting output.
- Reused the newly created Develop preview pipeline to reconstruct missing range
  sources instead of allocating a second preview pipeline during RAW open.
- Replaced Android lens-cache `expect` calls with a safe tuple match; an
  incomplete preview state can no longer panic during load finalization.
- Corrected object-mask download/inference failure routing so one appropriate
  error surface is used.
- Kept failed task acknowledgments intact when the active document changes.

### Android UI and notifications

- Android progress dialogs remain non-minimizable and foreground-modal; desktop
  retains minimization.
- Guarded RAW open and camera-profile changes while Android foreground work owns
  the limited GPU budget.
- Moved `TaskNotificationController` initialization to the start of Activity
  delegate setup and made JNI notification update/clear callbacks null-safe.
- Reset notification deduplication state when a new NativeActivity is installed,
  preventing a recreated Activity from suppressing its first progress update.
- Kept the notification permission request contextual and the notification
  update rate deduplicated/throttled.

## Validation executed

```text
pytest -q
417 passed

python scripts/check-source-tree.py
source tree contains only connected modules and tracked shader sources

python scripts/check-source-connectivity.py
source tree contains only connected modules and tracked shader sources

python -m compileall -q tests scripts regression
passed

AndroidManifest.xml and ic_notification.xml XML parsing
passed

javac -Xlint:all TaskNotificationController.java against local Android API stubs
passed
```

Additional static review performed:

- Full diff and review-only diff regenerated from the source trees.
- Added-line scan for `unsafe`, production `unwrap`/`expect`, `panic!`, `todo!`,
  and `unimplemented!`; remaining unwraps are confined to unit-test assertions.
- Modified-source duplicate-function and repeated-block scans.
- Added-function reference-count scan for dead wrappers.
- Rust delimiter-balance scan across the source tree.
- Unified-diff whitespace check after generated artifacts and caches were removed.

## Rust build status

The environment contains no `cargo`, `rustc`, `rustfmt`, or `clippy-driver`.
Therefore these required commands could not be executed here:

```text
cargo fmt --check
cargo check
cargo test
cargo clippy --all-targets --all-features
```

This is an environment/toolchain limitation. The build did not reach LibRaw,
Lensfun, ONNX Runtime, or other native dependency discovery.

## Remaining limitations

- Android notifications are only a progress mirror, not a foreground service.
  Leaving or closing AuRaw can suspend or terminate the operation.
- Android batch export still uses the Activity document bridge and temporarily
  loads each batch RAW into application document state. The modal UI prevents
  concurrent user opens, but this is not the same document isolation used by the
  desktop batch worker.
- Library AI-mask refresh still uses the main document loader and can temporarily
  replace the loaded document while processing library items.
- Model readiness is initially inferred from file existence. A corrupt existing
  model may be discovered only by the worker, at which point it can transition
  into a repair download outside the ideal preclassified path. A verified model
  readiness cache would be needed to remove that edge case without hashing large
  model files on the UI thread.
- Canonical AI-mask source capture retains its existing UI-thread GPU readback
  preflight.
- Lensfun and ONNX calls are cancellable between safe phases, not from inside the
  native call.
- Concurrent AI inference and export can still contend for CPU/GPU resources.

## Android export stability follow-up

The Android crash reported during Library batch export was caused by a renderer
write guard in `poll_load_worker()` remaining live until the end of the successful
load match arm. The batch completion callback immediately tried to acquire the
same epaint renderer lock again to discard the temporary preview, producing the
10-second debug deadlock panic. The lock is now scoped only around texture
replacement and is released before any document-owner callback runs.

Android Develop export also retained the full preview GPU pipeline while the
full-quality tiled export pipeline was being created. On the 384 MiB mobile GPU
budget this combined a roughly 154 MiB preview reservation with a roughly 237 MiB
export reservation and failed before the first tile. Single-image and batch
exports now share one Android preview-suspension path that:

- captures all export inputs before releasing preview resources;
- frees egui preview textures under the renderer lock;
- drops the main preview pipeline after releasing that lock;
- suppresses preview reconstruction while export or MediaStore publication is
  active; and
- rebuilds the Develop preview after a single-image export completes.

Library batch items do not rebuild the temporary Develop preview between images.
Export-start failures now propagate through a `Result`, cancel reserved direct
Android destinations, and retain the specific failure text instead of silently
continuing with a generic error.

Follow-up validation:

```text
pytest -q
419 passed

python scripts/check-source-tree.py
passed

python scripts/check-source-connectivity.py
passed

python -m compileall -q tests scripts regression
passed

git diff --no-index --check
no whitespace diagnostics
```

Rust compilation, rustfmt, Rust unit tests, and Clippy remain unexecuted because
this environment does not contain a Rust toolchain.
