package de.duecki.auraw;

import android.content.ContentResolver;
import android.content.SharedPreferences;

import java.io.File;

/**
 * Minimal Android platform surface required by storage-oriented delegates.
 *
 * <p>Keeping this dependency narrow lets storage code be exercised with a
 * controlled implementation and prevents it from accumulating Activity/UI
 * responsibilities. JNI still reaches the same delegates through
 * {@link AuRawActivity}; this is an internal dependency-injection boundary.
 */
interface AndroidStorageAccess {
    ContentResolver getContentResolver();

    SharedPreferences getSharedPreferences(String name, int mode);

    File getCacheDir();

    File getFilesDir();

    File getNoBackupFilesDir();

    File[] getExternalMediaDirs();

    String getPackageName();

    void runOnUiThread(Runnable action);
}

/** Activity-backed production implementation of {@link AndroidStorageAccess}. */
final class ActivityStorageAccess implements AndroidStorageAccess {
    private final AuRawActivity activity;

    ActivityStorageAccess(AuRawActivity activity) {
        this.activity = activity;
    }

    @Override
    public ContentResolver getContentResolver() {
        return activity.getContentResolver();
    }

    @Override
    public SharedPreferences getSharedPreferences(String name, int mode) {
        return activity.getSharedPreferences(name, mode);
    }

    @Override
    public File getCacheDir() {
        return activity.getCacheDir();
    }

    @Override
    public File getFilesDir() {
        return activity.getFilesDir();
    }

    @Override
    public File getNoBackupFilesDir() {
        return activity.getNoBackupFilesDir();
    }

    @Override
    public File[] getExternalMediaDirs() {
        return activity.getExternalMediaDirs();
    }

    @Override
    public String getPackageName() {
        return activity.getPackageName();
    }

    @Override
    public void runOnUiThread(Runnable action) {
        activity.runOnUiThread(action);
    }
}
