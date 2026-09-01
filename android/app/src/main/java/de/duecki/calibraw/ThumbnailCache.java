package de.duecki.calibraw;

import android.util.Log;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.Arrays;
import java.util.Locale;

final class ThumbnailCache {
    private static final String LOG_TAG = "CalibRaw";
    private static final int MAX_ENTRIES = 512;
    private static final long MAX_BYTES = 128L * 1024L * 1024L;

    private final AndroidStorageAccess storage;

    ThumbnailCache(AndroidStorageAccess storage) {
        this.storage = storage;
    }

    String rawPath(String uriText, long bytes, long modifiedSeconds, int maximumEdge)
            throws Exception {
        // v2 invalidates previews from before TIFF color/tone handling stabilized.
        return path(
                "raw-v2\n" + uriText + "\n" + bytes + "\n" + modifiedSeconds
                        + "\n" + maximumEdge,
                ".raw.jpg").getAbsolutePath();
    }

    String developedPath(String uriText) throws Exception {
        return path("developed\n" + uriText, ".developed.jpg").getAbsolutePath();
    }

    void copyDeveloped(String sourceUri, String destinationUri) throws Exception {
        File source = new File(developedPath(sourceUri));
        File sourceFingerprint = new File(source.getPath() + ".fingerprint");
        if (!source.isFile() || !sourceFingerprint.isFile()) {
            return;
        }
        File destination = new File(developedPath(destinationUri));
        File destinationFingerprint = new File(destination.getPath() + ".fingerprint");
        try {
            copyFile(source, destination);
            copyFile(sourceFingerprint, destinationFingerprint);
        } catch (Exception error) {
            deleteFile(destination);
            deleteFile(destinationFingerprint);
            throw error;
        }
    }

    void clearDeveloped(String uriText) {
        try {
            File thumbnail = new File(developedPath(uriText));
            deleteFile(thumbnail);
            deleteFile(new File(thumbnail.getPath() + ".fingerprint"));
        } catch (Exception error) {
            Log.w(LOG_TAG, "Could not clear developed thumbnail cache for " + uriText, error);
        }
    }

    void clear() {
        clearDirectory(persistentDirectory());
        clearDirectory(legacyDirectory());
    }

    long sizeBytes() {
        long persistent = directorySize(persistentDirectory());
        long legacy = directorySize(legacyDirectory());
        return persistent > Long.MAX_VALUE - legacy ? Long.MAX_VALUE : persistent + legacy;
    }

    private File path(String identity, String suffix) throws Exception {
        File directory = persistentDirectory();
        byte[] digest = MessageDigest.getInstance("SHA-256").digest(
                identity.getBytes(StandardCharsets.UTF_8));
        StringBuilder name = new StringBuilder();
        for (byte value : digest) {
            name.append(String.format(Locale.ROOT, "%02x", value & 0xff));
        }
        File cached = new File(directory, name.append(suffix).toString());
        migrateLegacyEntry(cached);
        touch(cached);
        trim(directory);
        return cached;
    }

    // Bounded no-backup storage survives cache eviction but is removed on uninstall.
    private File persistentDirectory() {
        File directory = new File(storage.getNoBackupFilesDir(), "library-thumbnails");
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create the persistent thumbnail cache");
        }
        return directory;
    }

    private File legacyDirectory() {
        return new File(storage.getCacheDir(), "library-thumbnails");
    }

    private static void clearDirectory(File directory) {
        if (!directory.exists()) {
            return;
        }
        File[] entries = directory.listFiles();
        if (entries == null) {
            throw new IllegalStateException("Could not inspect thumbnail cache " + directory);
        }
        for (File entry : entries) {
            if (!entry.isFile() || (!entry.delete() && entry.exists())) {
                throw new IllegalStateException("Could not clear thumbnail cache entry " + entry);
            }
        }
    }

    private static long directorySize(File directory) {
        if (!directory.isDirectory()) {
            return 0L;
        }
        File[] entries = directory.listFiles();
        if (entries == null) {
            return 0L;
        }
        long total = 0L;
        for (File entry : entries) {
            long bytes = entry.isDirectory() ? directorySize(entry) : Math.max(0L, entry.length());
            total = total > Long.MAX_VALUE - bytes ? Long.MAX_VALUE : total + bytes;
        }
        return total;
    }

    private void migrateLegacyEntry(File destination) {
        File legacyDirectory = legacyDirectory();
        if (!legacyDirectory.isDirectory()) {
            return;
        }
        migrateLegacyFile(new File(legacyDirectory, destination.getName()), destination);
        File fingerprint = new File(destination.getPath() + ".fingerprint");
        migrateLegacyFile(
                new File(legacyDirectory, destination.getName() + ".fingerprint"), fingerprint);
    }

    private static void migrateLegacyFile(File source, File destination) {
        if (destination.isFile() || !source.isFile()) {
            return;
        }
        try {
            AndroidStorageContract.moveOrCopyLegacyFile(source, destination, MAX_BYTES);
        } catch (Exception error) {
            Log.w(LOG_TAG, "Could not migrate legacy thumbnail cache entry", error);
        }
    }

    private static void touch(File cached) {
        if (!cached.isFile()) {
            return;
        }
        long now = System.currentTimeMillis();
        cached.setLastModified(now);
        File fingerprint = new File(cached.getPath() + ".fingerprint");
        if (fingerprint.isFile()) {
            fingerprint.setLastModified(now);
        }
    }

    static void trim(File directory) {
        File[] legacyPngs = directory.listFiles(
                (parent, name) -> name.endsWith(".png") || name.endsWith(".png.fingerprint"));
        if (legacyPngs != null) {
            for (File legacyPng : legacyPngs) {
                deleteFile(legacyPng);
            }
        }

        File[] fingerprints = directory.listFiles(
                (parent, name) -> name.endsWith(".developed.jpg.fingerprint"));
        if (fingerprints != null) {
            for (File fingerprint : fingerprints) {
                String name = fingerprint.getName();
                String thumbnailName = name.substring(0, name.length() - ".fingerprint".length());
                if (!new File(directory, thumbnailName).isFile()) {
                    deleteFile(fingerprint);
                }
            }
        }

        File[] thumbnails = directory.listFiles((parent, name) -> name.endsWith(".jpg"));
        if (thumbnails == null || thumbnails.length == 0) {
            return;
        }
        Arrays.sort(
                thumbnails,
                (left, right) -> Long.compare(left.lastModified(), right.lastModified()));

        int minimumRemovals = Math.max(0, thumbnails.length - MAX_ENTRIES);
        long totalBytes = directorySize(directory);
        for (int index = 0; index < thumbnails.length; index++) {
            if (index >= minimumRemovals && totalBytes <= MAX_BYTES) {
                break;
            }
            File thumbnail = thumbnails[index];
            File fingerprint = new File(thumbnail.getPath() + ".fingerprint");
            long removedBytes = Math.max(0L, thumbnail.length())
                    + Math.max(0L, fingerprint.length());
            deleteEntry(thumbnail);
            if (!thumbnail.exists() && !fingerprint.exists()) {
                totalBytes = Math.max(0L, totalBytes - removedBytes);
            } else {
                totalBytes = directorySize(directory);
            }
        }
    }

    private static void copyFile(File source, File destination) throws Exception {
        try (FileInputStream input = new FileInputStream(source);
             FileOutputStream output = new FileOutputStream(destination)) {
            BoundedStreams.copy(
                    input,
                    output,
                    MAX_BYTES,
                    "The document exceeds the " + MAX_BYTES + "-byte import limit");
            output.getFD().sync();
        }
    }

    private static void deleteEntry(File thumbnail) {
        deleteFile(thumbnail);
        deleteFile(new File(thumbnail.getPath() + ".fingerprint"));
    }

    private static void deleteFile(File file) {
        if (!file.delete() && file.exists()) {
            file.deleteOnExit();
        }
    }
}
