# Android background execution and live progress notifications

This document remains the design investigation for durable Android execution.
The notification-only phase is now implemented: AuRaw mirrors globally visible
task progress into an ongoing notification with Android 13+ permission handling.
No foreground service, WorkManager, UIDT, or process-lifecycle engine has been
added yet.

## Executive conclusion

A live notification that mirrors AuRaw's existing task progress is relatively
straightforward. Making exports and the complete FIFO queue continue reliably
when the user leaves the Activity is a substantially larger refactor because the
current scheduler, progress polling, Android document loading, and several final
publication steps are owned by `AurawApp` and driven from the eframe UI loop.

The recommended production direction is a same-process Android foreground
service backed by a UI-independent Rust task engine. Do not use a separate
Android process initially. A separate process would require IPC and fully
serializable task requests while also creating a second Vulkan/wgpu device and
increasing memory pressure on the exact devices that already have tight GPU
budgets.

## What Android supports

### Standard progress notification

All supported AuRaw Android versions can show an ongoing notification with:

- task title and phase
- determinate or indeterminate progress
- current image and completed/total counts
- a Cancel action through a `PendingIntent`
- a content intent that reopens AuRaw

The notification should be updated at a throttled cadence, approximately two to
four times per second, rather than for every tile or byte callback.

On Android 13 and newer, `POST_NOTIFICATIONS` controls whether a foreground-
service notification is visible in the notification drawer. A foreground service
may still start if the permission is denied, but the user generally sees it only
in Android's active-app/Task Manager surface.

### Android 16 progress and Live Update APIs

Android 16 adds `Notification.ProgressStyle` and promoted ongoing/Live Update
notifications. AuRaw could use `ProgressStyle` for richer segmented batch
progress on API 36 while retaining the normal notification progress bar on older
versions.

Promoted Live Update treatment is not guaranteed. It requires the promoted-
notification manifest permission, an ongoing eligible notification, a user/OEM
setting that permits promotion, and a use case accepted by system heuristics.
Photo export is user-initiated and ongoing, but is less time-sensitive than the
navigation, delivery, and rideshare examples in Android's guidance. AuRaw should
therefore treat promotion as optional presentation, not as the mechanism that
keeps work alive.

### Foreground service type

RAW export, lens processing, mask inference, and inpainting are best classified
as `mediaProcessing`. Apps targeting Android 15 or newer must declare the
service type and `FOREGROUND_SERVICE_MEDIA_PROCESSING` permission. Android gives
`mediaProcessing` foreground services a shared six-hour background allowance in
a rolling 24-hour period, with `Service.onTimeout()` handling required on modern
versions.

Model downloads are data transfer. They could use a `dataSync` foreground
service, or a user-initiated data-transfer job on versions that support it.
Keeping model downloads inside the same service is architecturally simpler, but
uses the separate `dataSync` six-hour allowance. UIDT is more policy-aligned for
large explicit downloads but would split scheduling between the Rust queue and
JobScheduler.

## Why adding a Service alone is insufficient in the current code

### The FIFO executor is UI-frame driven

`drive_background_tasks(frame)` is called from `src/app/eframe_impl.rs`. Worker
receivers are also polled from the frame/UI path. eframe does not continuously
run the application UI while the Activity is invisible. Consequently:

- the current native worker might continue for some time after Home is pressed;
- task progress would stop being consumed by the manager;
- completion would not advance the FIFO queue;
- Android batch export would not start the next item;
- publication and success/failure handling might not run.

The service therefore needs its own scheduler/event pump. Merely showing a
foreground-service notification around the existing `AurawApp` does not create a
reliable background queue.

### Task payloads are in-memory UI objects

`BackgroundAction` currently stores requests containing objects such as:

- `wgpu::Device` and `wgpu::Queue`
- `Arc<LoadedRaw>`
- complete mask and inpainting state
- UI/document generation identities

The queue lives in `AurawApp.background_actions`. These payloads cannot be put in
an Android `Intent`, reconstructed after process death, or transferred to a
separate process. A service-capable design needs serializable task descriptors:
input URI/path, sidecar generation, settings snapshot, output reservation, and
model paths. RAW decode and GPU resource construction must happen inside the
service-owned engine.

### Android batch export still depends on interactive document loading

Desktop batch export already has an isolated worker. Android batch export still
opens each library item through the Android document bridge and routes the
result through the application's normal loading state before starting the next
item. That design requires the Activity, JNI bridge, and frame polling to remain
alive.

For background execution, Android batch export must become document-isolated in
the same manner as desktop: capture stable library URIs or app-owned paths,
decode directly in the worker, and never install the batch RAW into Develop.

### Android context ownership is Activity-bound

The Rust side stores `android_activity::AndroidApp`, which represents a specific
NativeActivity instance. The android-activity contract states that after the
Activity's destroy event there is no associated Activity, and a replacement
Activity receives a new `AndroidApp` and a new `android_main()` call.

A long-lived service must use its own Java `Service`/application context for:

- notifications
- MediaStore finalization
- cache and files directories
- cancellation intents
- reopening the Activity

It must not rely on retaining the old Activity's `AndroidApp` globally.

### GPU ownership and surface loss

Current export requests clone the eframe-created wgpu device and queue. This can
work while the Activity is merely stopped and the device remains alive, but it
is not a robust service boundary. If the NativeActivity is destroyed, eframe and
its render state can be torn down while the export is still running.

The robust design is one headless service-owned wgpu instance/adapter/device
that does not depend on an Android window surface. The UI can either use a
separate preview device, or a more ambitious shared GPU engine can serve both UI
preview and background export. The separate-device approach is simpler but must
be budgeted carefully on mobile; the shared-engine approach is more efficient
but is a larger synchronization refactor.

## Recommended architecture

### Java layer

Add `AuRawTaskService extends Service` in the existing app process.

Responsibilities:

1. Create the notification channel.
2. Accept Start, Cancel, Pause/Stop, and Reconnect intents.
3. Call `startForegroundService()` from a visible user action and promote with
   `startForeground()` immediately.
4. Update one ongoing notification from Rust snapshots.
5. Route the notification Cancel action back to the service.
6. Stop foreground mode and `stopSelf()` after the queue becomes empty.
7. Implement `onTimeout()` for `mediaProcessing` and `dataSync`.
8. Expose application/service context JNI operations rather than Activity-only
   methods.

Use `START_NOT_STICKY` until queue journaling and restart semantics are complete.
After task descriptors are durable and idempotent, restart/redelivery can be
considered deliberately.

### Rust layer

Extract a process-level `BackgroundEngine` from `AurawApp`.

Responsibilities:

- own `BackgroundTaskManager` and typed queued descriptors;
- run a dedicated scheduler/event loop independent of egui frames;
- own worker receivers and advance FIFO automatically;
- publish snapshots to both Java notifications and any bound UI;
- own service-safe cancellation tokens;
- create/own headless GPU resources;
- validate document/generation identities before publishing results;
- journal queue and terminal state to app-private storage;
- clean incomplete MediaStore rows and temporary files after interruption.

`AurawApp` becomes a subscriber/controller. When the Activity is recreated it
attaches to the existing engine, receives current snapshots, and reconstructs
operation dialogs without restarting tasks.

### Persistent task descriptor

A durable export descriptor should include at least:

- stable task UUID, not the current in-memory counter only;
- source library URI/path and source fingerprint;
- captured sidecar/edit revision and settings snapshot;
- format and export settings;
- reserved MediaStore URI or a recoverable publication plan;
- task phase and current batch item;
- cancellation flag and cleanup metadata.

Do not journal raw pixel arrays, wgpu handles, egui textures, JNI local
references, or open file descriptors. Those are reconstructed for each service
run.

### Progress bridge

Use one normalized progress event shared by egui and Android:

- task ID
- name
- phase
- determinate fraction or units
- detail text
- waiting count
- cancellation availability

The notification bridge should coalesce events and update only when the phase,
integer percentage, current item, or a minimum time interval changes.

## Options compared

| Option | Leaves Activity | Survives Activity recreation | Survives process kill | Queue can advance | Complexity | Recommendation |
|---|---:|---:|---:|---:|---:|---|
| Notification mirror only | Sometimes | No | No | No | Low | Useful UI step only |
| Foreground service wrapping current app state | Usually | Fragile | No | Not without refactor | Medium | Prototype only |
| Same-process service + extracted Rust engine | Yes | Yes | With journal/restart policy | Yes | High | Recommended target |
| WorkManager long-running worker | Yes | Yes | Better scheduling semantics | Yes after major payload refactor | High | Better for downloads, less ideal for immediate GPU exports |
| Separate `android:process` service | Yes | Yes | With durable IPC protocol | Yes | Very high | Do not start here |

## Estimated effort

These are engineering estimates for this codebase, not implementation promises.

- Vertically centered desktop top-bar control: completed in this follow-up.
- Notification mirror for the current running task: roughly 1-3 engineering
  days, including channel, permission flow, JNI methods, throttling, and Cancel.
- Prototype that keeps one already-started export alive after Home: roughly
  1-2 weeks, with significant lifecycle/device testing and no process-death
  recovery.
- Production same-process service for single and batch export with Activity
  recreation, queue advancement, cancellation, and MediaStore cleanup: roughly
  3-6 weeks.
- Integrating lens work, model downloads, AI inference, inpainting, persistent
  recovery, and a full device/API test matrix: roughly 6-10 weeks total.
- Separate-process execution would likely exceed that and adds little benefit on
  memory-constrained phones.

The largest uncertainty is mobile Vulkan behavior while the NativeActivity
surface is terminated and whether a second headless device fits the app's GPU
budget across Adreno, Mali, and Samsung drivers.

## Suggested implementation sequence

1. **Implemented:** notification-only progress mirror and permission UX without
   changing execution ownership.
2. Extract task snapshots and event polling into a UI-independent Rust engine.
3. Convert Android batch export to isolated direct decode, matching desktop.
4. Add the same-process `mediaProcessing` foreground service and bind the UI.
5. Move MediaStore creation/finalization to service context.
6. Add durable queue journaling and interruption cleanup.
7. Add API 36 `ProgressStyle` as optional presentation.
8. Evaluate UIDT only for model downloads after the core service is stable.

## Test matrix required before release

- Android 8, 13, 15, and 16 behavior
- notification permission allowed and denied
- Home, screen off, app switch, split screen, and Activity recreation
- swipe AuRaw from Recents while service runs
- cancel from notification during decode, GPU tiles, encoding, and publication
- process kill during a pending MediaStore export and subsequent cleanup
- service timeout callbacks using Android's test overrides
- Adreno, Mali, and at least one Samsung device
- low-memory pressure while UI and service GPU devices coexist
- batch export followed by reopening AuRaw and reconnecting to the running task

## Primary references

- Android foreground services overview:
  https://developer.android.com/develop/background-work/services/fgs
- Launching a foreground service:
  https://developer.android.com/develop/background-work/services/fgs/launch
- Foreground service types:
  https://developer.android.com/develop/background-work/services/fgs/service-types
- Foreground service timeout behavior:
  https://developer.android.com/develop/background-work/services/fgs/timeout
- Android 16 Live Update notifications:
  https://developer.android.com/develop/ui/views/notifications/live-update
- Android 16 progress-centric notifications:
  https://developer.android.com/about/versions/16/features/progress-centric-notifications
- Notification runtime permission:
  https://developer.android.com/develop/ui/compose/notifications/notification-permission
- WorkManager long-running workers:
  https://developer.android.com/develop/background-work/background-tasks/persistent/how-to/long-running
- User-initiated data transfer jobs:
  https://developer.android.com/develop/background-work/background-tasks/uidt
- Android process lifecycle:
  https://developer.android.com/guide/components/activities/process-lifecycle
- android-activity lifecycle contract:
  https://docs.rs/android-activity/0.6.1/android_activity/
- eframe changelog/lifecycle behavior:
  https://github.com/emilk/egui/blob/master/crates/eframe/CHANGELOG.md
