package de.duecki.calibraw;

import android.Manifest;
import android.app.Activity;
import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.os.Build;

final class TaskNotificationController {
    static final int REQUEST_POST_NOTIFICATIONS = 1004;

    private static final String CHANNEL_ID = "calibraw_task_progress";
    private static final String CHANNEL_NAME = "Task progress";
    private static final String CHANNEL_DESCRIPTION =
            "Progress for exports, model downloads, and other long operations";
    private static final String PREFERENCES = "calibraw-notifications";
    private static final String ASKED_PERMISSION = "post-notifications-asked";
    private static final int NOTIFICATION_ID = 2001;

    private final Activity activity;
    private final NotificationManager notificationManager;
    private final SharedPreferences preferences;

    private PendingUpdate pendingUpdate;
    private boolean permissionRequestInFlight;

    TaskNotificationController(Activity activity) {
        this.activity = activity;
        notificationManager =
                (NotificationManager) activity.getSystemService(Context.NOTIFICATION_SERVICE);
        preferences = activity.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE);
        createChannel();
        if (notificationManager != null) {
            notificationManager.cancel(NOTIFICATION_ID);
        }
    }

    void update(
            String title,
            String phase,
            String detail,
            int progressPercent,
            boolean indeterminate,
            int queuedCount) {
        PendingUpdate update = new PendingUpdate(
                safe(title, "CalibRaw task"),
                safe(phase, "Working…"),
                detail == null ? "" : detail,
                Math.max(0, Math.min(100, progressPercent)),
                indeterminate,
                Math.max(0, queuedCount));
        activity.runOnUiThread(() -> {
            pendingUpdate = update;
            if (canPostNotifications()) {
                post(update);
            } else {
                requestPermissionOnce();
            }
        });
    }

    void clear() {
        activity.runOnUiThread(() -> {
            pendingUpdate = null;
            if (notificationManager != null) {
                notificationManager.cancel(NOTIFICATION_ID);
            }
        });
    }

    void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        if (requestCode != REQUEST_POST_NOTIFICATIONS) {
            return;
        }
        permissionRequestInFlight = false;
        if (grantResults.length == 0) {
            return;
        }
        preferences.edit().putBoolean(ASKED_PERMISSION, true).apply();
        if (grantResults[0] == PackageManager.PERMISSION_GRANTED && pendingUpdate != null) {
            post(pendingUpdate);
        }
    }

    private void createChannel() {
        if (notificationManager == null || Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
            return;
        }
        NotificationChannel channel = new NotificationChannel(
                CHANNEL_ID, CHANNEL_NAME, NotificationManager.IMPORTANCE_LOW);
        channel.setDescription(CHANNEL_DESCRIPTION);
        channel.setShowBadge(false);
        notificationManager.createNotificationChannel(channel);
    }

    private boolean canPostNotifications() {
        return Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU
                || activity.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS)
                        == PackageManager.PERMISSION_GRANTED;
    }

    private void requestPermissionOnce() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU
                || permissionRequestInFlight
                || preferences.getBoolean(ASKED_PERMISSION, false)) {
            return;
        }
        permissionRequestInFlight = true;
        activity.requestPermissions(
                new String[] {Manifest.permission.POST_NOTIFICATIONS},
                REQUEST_POST_NOTIFICATIONS);
    }

    private void post(PendingUpdate update) {
        if (notificationManager == null || !canPostNotifications()) {
            return;
        }

        Intent reopenIntent = new Intent(activity, CalibRawActivity.class)
                .addFlags(Intent.FLAG_ACTIVITY_REORDER_TO_FRONT | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        PendingIntent contentIntent = PendingIntent.getActivity(
                activity,
                0,
                reopenIntent,
                PendingIntent.FLAG_UPDATE_CURRENT | PendingIntent.FLAG_IMMUTABLE);

        String progressSuffix = update.indeterminate
                ? ""
                : " · " + update.progressPercent + "%";
        String queueSuffix = update.queuedCount > 0
                ? " · +" + update.queuedCount + " waiting"
                : "";
        String compactText = update.phase + progressSuffix + queueSuffix;
        String detailText = update.detail.isEmpty()
                ? compactText
                : compactText + "\n" + update.detail;
        String expandedText = detailText
                + "\nKeep CalibRaw open in the foreground until this operation finishes. Leaving or closing the app may stop it.";

        Notification.Builder builder = new Notification.Builder(activity, CHANNEL_ID)
                .setSmallIcon(R.drawable.ic_notification)
                .setContentTitle(update.title)
                .setContentText(compactText)
                .setStyle(new Notification.BigTextStyle().bigText(expandedText))
                .setSubText("Keep CalibRaw open")
                .setContentIntent(contentIntent)
                .setCategory(Notification.CATEGORY_PROGRESS)
                .setOnlyAlertOnce(true)
                .setOngoing(true)
                .setAutoCancel(false)
                .setShowWhen(false);
        if (update.indeterminate) {
            builder.setProgress(0, 0, true);
        } else {
            builder.setProgress(100, update.progressPercent, false);
        }
        notificationManager.notify(NOTIFICATION_ID, builder.build());
    }

    private static String safe(String value, String fallback) {
        return value == null || value.isEmpty() ? fallback : value;
    }

    private static final class PendingUpdate {
        final String title;
        final String phase;
        final String detail;
        final int progressPercent;
        final boolean indeterminate;
        final int queuedCount;

        PendingUpdate(
                String title,
                String phase,
                String detail,
                int progressPercent,
                boolean indeterminate,
                int queuedCount) {
            this.title = title;
            this.phase = phase;
            this.detail = detail;
            this.progressPercent = progressPercent;
            this.indeterminate = indeterminate;
            this.queuedCount = queuedCount;
        }
    }
}
