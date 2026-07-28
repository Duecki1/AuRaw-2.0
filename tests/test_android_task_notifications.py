from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MANIFEST = (ROOT / "android/app/src/main/AndroidManifest.xml").read_text(encoding="utf-8")
ACTIVITY = (
    ROOT / "android/app/src/main/java/de/duecki/auraw/AuRawActivity.java"
).read_text(encoding="utf-8")
CONTROLLER = (
    ROOT / "android/app/src/main/java/de/duecki/auraw/TaskNotificationController.java"
).read_text(encoding="utf-8")
ANDROID_RS = (ROOT / "src/android.rs").read_text(encoding="utf-8")
RUNTIME = (ROOT / "src/app/background_task_runtime.rs").read_text(encoding="utf-8")
EFRAME = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")


def test_android_notification_permission_is_declared_and_requested_contextually():
    assert 'android.permission.POST_NOTIFICATIONS' in MANIFEST
    assert 'Manifest.permission.POST_NOTIFICATIONS' in CONTROLLER
    assert 'Build.VERSION_CODES.TIRAMISU' in CONTROLLER
    assert 'activity.requestPermissions(' in CONTROLLER
    assert 'REQUEST_POST_NOTIFICATIONS = 1004' in CONTROLLER
    assert 'post-notifications-asked' in CONTROLLER


def test_android_task_notification_has_live_progress_and_reopens_auraw():
    assert 'NotificationChannel(' in CONTROLLER
    assert 'NotificationManager.IMPORTANCE_LOW' in CONTROLLER
    assert '.setProgress(100, update.progressPercent, false)' in CONTROLLER
    assert '.setProgress(0, 0, true)' in CONTROLLER
    assert '" · " + update.progressPercent + "%"' in CONTROLLER
    assert '.setOnlyAlertOnce(true)' in CONTROLLER
    assert '.setOngoing(true)' in CONTROLLER
    assert 'AuRawActivity.class' in CONTROLLER
    assert 'R.drawable.ic_notification' in CONTROLLER
    assert '+" + update.queuedCount + " waiting"' in CONTROLLER


def test_rust_task_snapshots_are_bridged_to_one_deduplicated_notification():
    assert 'struct TaskNotificationState' in ANDROID_RS
    assert 'static TASK_NOTIFICATION_STATE' in ANDROID_RS
    assert 'TASK_NOTIFICATION_MIN_UPDATE_INTERVAL' in ANDROID_RS
    assert 'Duration::from_millis(250)' in ANDROID_RS
    assert 'updateBackgroundTaskNotification' in ANDROID_RS
    assert 'clearBackgroundTaskNotification' in ANDROID_RS
    assert 'fn sync_android_task_notification(&self)' in RUNTIME
    assert 'TaskProgressValue::Fraction' in RUNTIME
    assert 'TaskProgressValue::Units' in RUNTIME
    assert 'self.global_background_task_snapshots()' in RUNTIME
    assert 'self.global_background_queued_count()' in RUNTIME
    assert 'self.sync_android_task_notification();' in EFRAME


def test_notification_is_cleared_when_the_queue_or_activity_ends():
    assert 'taskNotificationController.clear();' in ACTIVITY
    assert 'protected void onDestroy()' in ACTIVITY
    assert 'clear_background_task_notification(&self.android_app)' in RUNTIME
    assert 'clear_background_task_notification(&self.android_app)' in EFRAME


def test_android_warns_that_current_tasks_are_not_background_services_yet():
    note = 'Keep AuRaw open in the foreground until the operation finishes. Leaving or closing the app may stop it.'
    assert note in EFRAME
    assert 'Keep AuRaw open in the foreground until this operation finishes. Leaving or closing the app may stop it.' in CONTROLLER
