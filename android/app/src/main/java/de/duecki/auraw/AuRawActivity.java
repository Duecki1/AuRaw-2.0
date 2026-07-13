package de.duecki.auraw;

import android.Manifest;
import android.app.NativeActivity;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.media.MediaScannerConnection;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.provider.MediaStore;
import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.Locale;

public final class AuRawActivity extends NativeActivity {
    private static final int OPEN_RAW_DOCUMENT = 1001;
    private static final int WRITE_EXPORT_PERMISSION = 1002;
    private static final long MAX_RAW_IMPORT_BYTES = 2_000_000_000L;

    private String pendingExportPath;
    private String pendingExportName;

    static {
        System.loadLibrary("auraw");
    }

    private static native void nativeOnFilePicked(
            String cachedPath,
            String displayName,
            String error);

    private static native void nativeOnExportPublished(
            String location,
            String error);

    /** Called from Rust's egui button. */
    public void openRawDocument() {
        runOnUiThread(this::launchRawDocumentPicker);
    }

    private void launchRawDocumentPicker() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("*/*");
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        startActivityForResult(intent, OPEN_RAW_DOCUMENT);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != OPEN_RAW_DOCUMENT) {
            return;
        }
        if (resultCode != RESULT_OK || data == null || data.getData() == null) {
            nativeOnFilePicked("", "", "");
            return;
        }

        Uri uri = data.getData();
        String displayName = queryDisplayName(uri);
        new Thread(
                () -> importDocument(uri, displayName),
                "AuRaw document import").start();
    }

    private void importDocument(Uri uri, String displayName) {
        File imported = null;
        try {
            Long declaredSize = queryDocumentSize(uri);
            if (declaredSize != null && declaredSize > MAX_RAW_IMPORT_BYTES) {
                throw new IllegalStateException(
                        "The selected RAW is " + declaredSize
                                + " bytes; the Android import limit is "
                                + MAX_RAW_IMPORT_BYTES);
            }
            String suffix = suffixFor(displayName);
            imported = File.createTempFile("auraw-import-", suffix, getCacheDir());
            try (InputStream input = getContentResolver().openInputStream(uri);
                 FileOutputStream output = new FileOutputStream(imported)) {
                if (input == null) {
                    throw new IllegalStateException("The document provider returned no input stream");
                }
                byte[] buffer = new byte[1024 * 1024];
                copy(input, output, MAX_RAW_IMPORT_BYTES);
            }
            nativeOnFilePicked(imported.getAbsolutePath(), displayName, "");
        } catch (Exception error) {
            if (imported != null && !imported.delete() && imported.exists()) {
                imported.deleteOnExit();
            }
            nativeOnFilePicked("", displayName, error.toString());
        }
    }

    /** Publishes a completed cache PNG to Pictures/AuRaw without showing a picker. */
    public void publishPng(String cachedPath, String displayName) {
        runOnUiThread(() -> beginPublishPng(cachedPath, displayName));
    }

    private void beginPublishPng(String cachedPath, String displayName) {
        if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.P
                && checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE)
                != PackageManager.PERMISSION_GRANTED) {
            pendingExportPath = cachedPath;
            pendingExportName = displayName;
            requestPermissions(
                    new String[]{Manifest.permission.WRITE_EXTERNAL_STORAGE},
                    WRITE_EXPORT_PERMISSION);
            return;
        }
        startPublishThread(cachedPath, displayName);
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode,
            String[] permissions,
            int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode != WRITE_EXPORT_PERMISSION) {
            return;
        }
        String cachedPath = pendingExportPath;
        String displayName = pendingExportName;
        pendingExportPath = null;
        pendingExportName = null;
        if (grantResults.length > 0
                && grantResults[0] == PackageManager.PERMISSION_GRANTED
                && cachedPath != null) {
            startPublishThread(cachedPath, displayName);
        } else {
            if (cachedPath != null) {
                new File(cachedPath).delete();
            }
            nativeOnExportPublished(
                    "",
                    "Storage permission is required to export on Android 8 and 9");
        }
    }

    private void startPublishThread(String cachedPath, String displayName) {
        new Thread(
                () -> publishPngInBackground(cachedPath, displayName),
                "AuRaw PNG publish").start();
    }

    private void publishPngInBackground(String cachedPath, String requestedName) {
        File cachedFile = new File(cachedPath);
        String displayName = safePngName(requestedName);
        try {
            String location;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                location = publishPngScoped(cachedFile, displayName);
            } else {
                location = publishPngLegacy(cachedFile, displayName);
            }
            nativeOnExportPublished(location, "");
        } catch (Exception error) {
            nativeOnExportPublished("", error.toString());
        } finally {
            if (!cachedFile.delete() && cachedFile.exists()) {
                cachedFile.deleteOnExit();
            }
        }
    }

    private String publishPngScoped(File cachedFile, String displayName) throws Exception {
        ContentResolver resolver = getContentResolver();
        ContentValues values = new ContentValues();
        values.put(MediaStore.Images.Media.DISPLAY_NAME, displayName);
        values.put(MediaStore.Images.Media.MIME_TYPE, "image/png");
        values.put(
                MediaStore.Images.Media.RELATIVE_PATH,
                Environment.DIRECTORY_PICTURES + "/AuRaw");
        values.put(MediaStore.Images.Media.IS_PENDING, 1);

        Uri uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values);
        if (uri == null) {
            throw new IllegalStateException("Android MediaStore could not create the image");
        }
        boolean published = false;
        try {
            try (InputStream input = new FileInputStream(cachedFile);
                 OutputStream output = resolver.openOutputStream(uri, "w")) {
                if (output == null) {
                    throw new IllegalStateException("Android MediaStore returned no output stream");
                }
                copy(input, output, Long.MAX_VALUE);
            }
            values.clear();
            values.put(MediaStore.Images.Media.IS_PENDING, 0);
            if (resolver.update(uri, values, null, null) <= 0) {
                throw new IllegalStateException("Android MediaStore could not publish the image");
            }
            published = true;
            return Environment.DIRECTORY_PICTURES + "/AuRaw/" + displayName;
        } finally {
            if (!published) {
                resolver.delete(uri, null, null);
            }
        }
    }

    @SuppressWarnings("deprecation")
    private String publishPngLegacy(File cachedFile, String displayName) throws Exception {
        File pictures = Environment.getExternalStoragePublicDirectory(
                Environment.DIRECTORY_PICTURES);
        File directory = new File(pictures, "AuRaw");
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create " + directory);
        }
        File destination = uniqueFile(directory, displayName);
        try (InputStream input = new FileInputStream(cachedFile);
             OutputStream output = new FileOutputStream(destination)) {
            copy(input, output, Long.MAX_VALUE);
        }
        MediaScannerConnection.scanFile(
                this,
                new String[]{destination.getAbsolutePath()},
                new String[]{"image/png"},
                null);
        return destination.getAbsolutePath();
    }

    private static File uniqueFile(File directory, String displayName) {
        File candidate = new File(directory, displayName);
        if (!candidate.exists()) {
            return candidate;
        }
        String stem = displayName.substring(0, displayName.length() - 4);
        for (int suffix = 1; ; suffix++) {
            candidate = new File(directory, stem + "-" + suffix + ".png");
            if (!candidate.exists()) {
                return candidate;
            }
        }
    }

    private static String safePngName(String requestedName) {
        String name = requestedName == null ? "AuRaw-export.png" : requestedName;
        name = name.replaceAll("[^A-Za-z0-9._-]", "_");
        if (name.isEmpty()) {
            name = "AuRaw-export.png";
        }
        if (!name.toLowerCase(Locale.ROOT).endsWith(".png")) {
            name += ".png";
        }
        return name;
    }

    private static void copy(InputStream input, OutputStream output, long maximumBytes)
            throws Exception {
        byte[] buffer = new byte[1024 * 1024];
        long copied = 0;
        while (true) {
            int count = input.read(buffer);
            if (count < 0) {
                break;
            }
            if (count == 0) {
                // Some ContentProvider streams are allowed to make no
                // progress. A one-byte read either advances or reaches EOF,
                // avoiding an unbounded zero-byte loop.
                int value = input.read();
                if (value < 0) {
                    break;
                }
                copied = checkedCopyLength(copied, 1, maximumBytes);
                output.write(value);
                continue;
            }
            copied = checkedCopyLength(copied, count, maximumBytes);
            output.write(buffer, 0, count);
        }
    }

    private static long checkedCopyLength(long copied, int count, long maximumBytes) {
        if (count < 0 || copied > maximumBytes - count) {
            throw new IllegalStateException(
                    "The document exceeds the " + maximumBytes + "-byte import limit");
        }
        return copied + count;
    }

    private Long queryDocumentSize(Uri uri) {
        try (Cursor cursor = getContentResolver().query(
                uri,
                new String[]{OpenableColumns.SIZE},
                null,
                null,
                null)) {
            if (cursor != null && cursor.moveToFirst()) {
                int column = cursor.getColumnIndex(OpenableColumns.SIZE);
                if (column >= 0 && !cursor.isNull(column)) {
                    long size = cursor.getLong(column);
                    if (size >= 0) {
                        return size;
                    }
                }
            }
        } catch (Exception ignored) {
            // The streaming limit remains authoritative when metadata is
            // absent, stale, or unavailable from the provider.
        }
        return null;
    }

    private String queryDisplayName(Uri uri) {
        try (Cursor cursor = getContentResolver().query(
                uri,
                new String[]{OpenableColumns.DISPLAY_NAME},
                null,
                null,
                null)) {
            if (cursor != null && cursor.moveToFirst()) {
                int column = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                if (column >= 0) {
                    String name = cursor.getString(column);
                    if (name != null && !name.isEmpty()) {
                        return name;
                    }
                }
            }
        } catch (Exception ignored) {
            // The URI itself is still readable even if this optional query fails.
        }
        return "selected RAW";
    }

    private static String suffixFor(String displayName) {
        int dot = displayName.lastIndexOf('.');
        if (dot < 0 || dot >= displayName.length() - 1) {
            return ".raw";
        }
        String suffix = displayName.substring(dot).toLowerCase(Locale.ROOT);
        return suffix.matches("\\.[a-z0-9]{1,10}") ? suffix : ".raw";
    }
}
