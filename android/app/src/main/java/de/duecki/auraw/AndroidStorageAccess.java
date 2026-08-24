package de.duecki.auraw;

import android.content.ContentResolver;
import android.content.SharedPreferences;

import java.io.File;

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
