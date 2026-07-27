package de.duecki.auraw;

import android.Manifest;
import android.content.ContentResolver;
import android.content.ContentValues;
import android.content.pm.PackageManager;
import android.media.MediaScannerConnection;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.os.ParcelFileDescriptor;
import android.provider.MediaStore;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.Locale;

/** Publishes completed exports through MediaStore or the Android 8/9 legacy path. */
final class ExportPublisher {
    static final int WRITE_EXPORT_PERMISSION = 1002;
    private static final String EXPORT_RELATIVE_PATH =
            Environment.DIRECTORY_PICTURES + "/AuRaw";

    interface Callbacks {
        void onExportPublished(String location, String error);
    }

    private final AuRawActivity activity;
    private final Callbacks callbacks;
    private String pendingExportPath;
    private String pendingExportName;
    private String pendingExportMimeType;

    ExportPublisher(AuRawActivity activity, Callbacks callbacks) {
        this.activity = activity;
        this.callbacks = callbacks;
    }

    /** Creates a pending MediaStore destination and transfers its writable fd to Rust. */
    String createPendingExport(String requestedName, String mimeType) throws Exception {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            return "";
        }
        String normalizedMime = normalizeExportMimeType(mimeType);
        String displayName = safeImageName(requestedName, normalizedMime);
        ContentResolver resolver = activity.getContentResolver();
        ContentValues values = new ContentValues();
        values.put(MediaStore.Images.Media.DISPLAY_NAME, displayName);
        values.put(MediaStore.Images.Media.MIME_TYPE, normalizedMime);
        values.put(
                MediaStore.Images.Media.RELATIVE_PATH,
                EXPORT_RELATIVE_PATH);
        values.put(MediaStore.Images.Media.IS_PENDING, 1);
        Uri uri = resolver.insert(MediaStore.Images.Media.EXTERNAL_CONTENT_URI, values);
        if (uri == null) {
            throw new IllegalStateException("Android MediaStore could not create the image");
        }
        boolean transferred = false;
        try {
            ParcelFileDescriptor descriptor = resolver.openFileDescriptor(uri, "w");
            if (descriptor == null) {
                throw new IllegalStateException("Android MediaStore returned no file descriptor");
            }
            int fd;
            try {
                fd = descriptor.detachFd();
                transferred = true;
            } finally {
                descriptor.close();
            }
            String location = EXPORT_RELATIVE_PATH + "/" + displayName;
            return fd + "\t" + uri + "\t" + location;
        } finally {
            if (!transferred) {
                resolver.delete(uri, null, null);
            }
        }
    }

    /** Publishes or deletes a MediaStore destination previously created above. */
    void finishPendingExport(String uriText, int successFlag) throws Exception {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q || uriText == null || uriText.isEmpty()) {
            return;
        }
        ContentResolver resolver = activity.getContentResolver();
        Uri uri = Uri.parse(uriText);
        boolean success = successFlag != 0;
        if (!success) {
            resolver.delete(uri, null, null);
            return;
        }
        ContentValues values = new ContentValues();
        values.put(MediaStore.Images.Media.IS_PENDING, 0);
        if (resolver.update(uri, values, null, null) <= 0) {
            resolver.delete(uri, null, null);
            throw new IllegalStateException("Android MediaStore could not publish the image");
        }
    }

    /** Publishes a completed cache image to Pictures/AuRaw without showing a picker. */
    void publishImage(String cachedPath, String displayName, String mimeType) {
        activity.runOnUiThread(() -> beginPublishImage(cachedPath, displayName, mimeType));
    }

    /** Backward-compatible entry point retained for older native builds. */
    void publishPng(String cachedPath, String displayName) {
        publishImage(cachedPath, displayName, "image/png");
    }

    private void beginPublishImage(String cachedPath, String displayName, String mimeType) {
        String normalizedMime = normalizeExportMimeType(mimeType);
        if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.P
                && activity.checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE)
                != PackageManager.PERMISSION_GRANTED) {
            pendingExportPath = cachedPath;
            pendingExportName = displayName;
            pendingExportMimeType = normalizedMime;
            activity.requestPermissions(
                    new String[]{Manifest.permission.WRITE_EXTERNAL_STORAGE},
                    WRITE_EXPORT_PERMISSION);
            return;
        }
        startPublishThread(cachedPath, displayName, normalizedMime);
    }


    private void startPublishThread(String cachedPath, String displayName, String mimeType) {
        new Thread(
                () -> publishImageInBackground(cachedPath, displayName, mimeType),
                "AuRaw image publish").start();
    }

    private void publishImageInBackground(
            String cachedPath,
            String requestedName,
            String mimeType) {
        File cachedFile = new File(cachedPath);
        String normalizedMime = normalizeExportMimeType(mimeType);
        String displayName = safeImageName(requestedName, normalizedMime);
        try {
            String location;
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                location = publishImageScoped(cachedFile, displayName, normalizedMime);
            } else {
                location = publishImageLegacy(cachedFile, displayName, normalizedMime);
            }
            callbacks.onExportPublished(location, "");
        } catch (Exception error) {
            callbacks.onExportPublished("", error.toString());
        } finally {
            if (!cachedFile.delete() && cachedFile.exists()) {
                cachedFile.deleteOnExit();
            }
        }
    }

    private String publishImageScoped(
            File cachedFile,
            String displayName,
            String mimeType) throws Exception {
        ContentResolver resolver = activity.getContentResolver();
        ContentValues values = new ContentValues();
        values.put(MediaStore.Images.Media.DISPLAY_NAME, displayName);
        values.put(MediaStore.Images.Media.MIME_TYPE, mimeType);
        values.put(
                MediaStore.Images.Media.RELATIVE_PATH,
                EXPORT_RELATIVE_PATH);
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
            return EXPORT_RELATIVE_PATH + "/" + displayName;
        } finally {
            if (!published) {
                resolver.delete(uri, null, null);
            }
        }
    }

    @SuppressWarnings("deprecation")
    private String publishImageLegacy(
            File cachedFile,
            String displayName,
            String mimeType) throws Exception {
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
                activity,
                new String[]{destination.getAbsolutePath()},
                new String[]{mimeType},
                null);
        return destination.getAbsolutePath();
    }

    private static File uniqueFile(File directory, String displayName) {
        File candidate = new File(directory, displayName);
        if (!candidate.exists()) {
            return candidate;
        }
        int dot = displayName.lastIndexOf('.');
        String stem = dot > 0 ? displayName.substring(0, dot) : displayName;
        String extension = dot > 0 ? displayName.substring(dot) : "";
        for (int suffix = 1; ; suffix++) {
            candidate = new File(directory, stem + "-" + suffix + extension);
            if (!candidate.exists()) {
                return candidate;
            }
        }
    }

    private static String normalizeExportMimeType(String mimeType) {
        return "image/jpeg".equalsIgnoreCase(mimeType) ? "image/jpeg" : "image/png";
    }

    private static String safeImageName(String requestedName, String mimeType) {
        boolean jpeg = "image/jpeg".equalsIgnoreCase(mimeType);
        String extension = jpeg ? ".jpg" : ".png";
        String fallback = jpeg ? "AuRaw-export.jpg" : "AuRaw-export.png";
        String name = requestedName == null ? fallback : requestedName;
        name = name.replaceAll("[^A-Za-z0-9._-]", "_");
        if (name.isEmpty()) {
            name = fallback;
        }
        String lower = name.toLowerCase(Locale.ROOT);
        if (jpeg) {
            if (!lower.endsWith(".jpg") && !lower.endsWith(".jpeg")) {
                name += extension;
            }
        } else if (!lower.endsWith(extension)) {
            name += extension;
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

    boolean onRequestPermissionsResult(
            int requestCode,
            String[] permissions,
            int[] grantResults) {
        if (requestCode != WRITE_EXPORT_PERMISSION) {
            return false;
        }
        String cachedPath = pendingExportPath;
        String displayName = pendingExportName;
        String mimeType = pendingExportMimeType;
        pendingExportPath = null;
        pendingExportName = null;
        pendingExportMimeType = null;
        if (grantResults.length > 0
                && grantResults[0] == PackageManager.PERMISSION_GRANTED
                && cachedPath != null) {
            startPublishThread(cachedPath, displayName, normalizeExportMimeType(mimeType));
        } else {
            if (cachedPath != null) {
                new File(cachedPath).delete();
            }
            callbacks.onExportPublished(
                    "",
                    "Storage permission is required to export on Android 8 and 9");
        }
        return true;
    }
}
