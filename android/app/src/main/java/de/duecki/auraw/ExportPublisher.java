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
import java.util.concurrent.atomic.AtomicReference;

/** Publishes completed exports through MediaStore or the Android 8/9 legacy path. */
final class ExportPublisher {
    static final int WRITE_EXPORT_PERMISSION = 1002;
    private static final String EXPORT_RELATIVE_PATH =
            AndroidStorageContract.exportRelativePath(Environment.DIRECTORY_PICTURES);

    interface Callbacks {
        void onExportPublished(String location, String error);
    }

    private final AuRawActivity activity;
    private final Callbacks callbacks;
    /** Permission callbacks and publish requests may arrive from different bridge threads. */
    private final AtomicReference<PendingLegacyExport> pendingLegacyExport = new AtomicReference<>();

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
            int fd = NativeFileDescriptors.detach(
                    descriptor, "Android MediaStore returned no file descriptor");
            transferred = true;
            String location = AndroidStorageContract.exportLocation(
                    Environment.DIRECTORY_PICTURES, displayName);
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

    private void beginPublishImage(String cachedPath, String displayName, String mimeType) {
        String normalizedMime = normalizeExportMimeType(mimeType);
        if (Build.VERSION.SDK_INT <= Build.VERSION_CODES.P
                && activity.checkSelfPermission(Manifest.permission.WRITE_EXTERNAL_STORAGE)
                != PackageManager.PERMISSION_GRANTED) {
            PendingLegacyExport replaced = pendingLegacyExport.getAndSet(
                    new PendingLegacyExport(cachedPath, displayName, normalizedMime));
            if (replaced != null) {
                deleteCachedExport(replaced.cachedPath);
                callbacks.onExportPublished(
                        "", "A newer export replaced the pending permission request");
            }
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
                BoundedStreams.copy(input, output, Long.MAX_VALUE, "Export is too large");
            }
            values.clear();
            values.put(MediaStore.Images.Media.IS_PENDING, 0);
            if (resolver.update(uri, values, null, null) <= 0) {
                throw new IllegalStateException("Android MediaStore could not publish the image");
            }
            published = true;
            return AndroidStorageContract.exportLocation(
                    Environment.DIRECTORY_PICTURES, displayName);
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
            BoundedStreams.copy(input, output, Long.MAX_VALUE, "Export is too large");
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
        return AndroidStorageContract.normalizeExportMimeType(mimeType);
    }

    private static String safeImageName(String requestedName, String mimeType) {
        return AndroidStorageContract.safeImageName(requestedName, mimeType);
    }

    boolean onRequestPermissionsResult(
            int requestCode,
            String[] permissions,
            int[] grantResults) {
        if (requestCode != WRITE_EXPORT_PERMISSION) {
            return false;
        }
        PendingLegacyExport pending = pendingLegacyExport.getAndSet(null);
        if (grantResults.length > 0
                && grantResults[0] == PackageManager.PERMISSION_GRANTED
                && pending != null) {
            startPublishThread(
                    pending.cachedPath,
                    pending.displayName,
                    normalizeExportMimeType(pending.mimeType));
        } else {
            if (pending != null) {
                deleteCachedExport(pending.cachedPath);
            }
            callbacks.onExportPublished(
                    "",
                    "Storage permission is required to export on Android 8 and 9");
        }
        return true;
    }

    private static void deleteCachedExport(String cachedPath) {
        File cached = new File(cachedPath);
        if (!cached.delete() && cached.exists()) {
            cached.deleteOnExit();
        }
    }

    private static final class PendingLegacyExport {
        final String cachedPath;
        final String displayName;
        final String mimeType;

        PendingLegacyExport(String cachedPath, String displayName, String mimeType) {
            this.cachedPath = cachedPath;
            this.displayName = displayName;
            this.mimeType = mimeType;
        }
    }
}
