package de.duecki.auraw;

import android.content.ClipData;
import android.content.ContentResolver;
import android.content.ContentUris;
import android.content.ContentValues;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Build;
import android.os.Environment;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import android.provider.MediaStore;
import android.provider.OpenableColumns;
import android.util.Log;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashSet;
import java.util.Locale;
import java.util.Set;

/** Owns RAW import, library, sidecar, thumbnail-cache, and migration storage operations. */
final class StorageManager {
    private static final String LOG_TAG = "AuRaw";
    private static final long MAX_RAW_IMPORT_BYTES = 2_000_000_000L;
    private static final long MAX_SIDECAR_BYTES = 32L * 1024L * 1024L;
    private static final int MAX_RAW_LIBRARY_FILES = 20_000;
    private static final int MAX_THUMBNAIL_CACHE_ENTRIES = 512;
    private static final long MAX_THUMBNAIL_CACHE_BYTES = 128L * 1024L * 1024L;
    private static final long STALE_TEMP_FILE_AGE_MS = 24L * 60L * 60L * 1000L;
    private static final String LEGACY_MEDIASTORE_RAW_RELATIVE_PATH =
            Environment.DIRECTORY_DOWNLOADS + "/AuRaw/";
    private static final String RAW_LIBRARY_DIRECTORY_NAME = ".library";
    private static final String PICKER_PREFERENCES = "auraw-picker-locations";
    private static final String RAW_PICKER_URI_KEY = "raw-document-uri";
    private static final Set<String> RAW_SUFFIXES = new HashSet<>(Arrays.asList(
            "3fr", "ari", "arw", "bay", "bmq", "cap", "cine", "cr2", "cr3", "crw",
            "cs1", "dc2", "dcr", "dcs", "dng", "drf", "eip", "erf", "fff", "gpr",
            "iiq", "k25", "kc2", "kdc", "mdc", "mef", "mos", "mrw", "nef", "nrw",
            "obm", "orf", "pef", "ptx", "pxn", "qtk", "r3d", "raf", "raw", "rdc",
            "rw2", "rwl", "rwz", "sr2", "srf", "srw", "sti", "x3f"));

    interface Callbacks {
        void onFilePicked(
                String cachedPath,
                String displayName,
                String libraryUri,
                String error,
                boolean temporary);

        void onFilePickedFd(int fd, String displayName, String libraryUri, String error);

        void onImportBatchFinished(int importedCount, int failedCount, String errors);
    }

    private final AuRawActivity activity;
    private final Callbacks callbacks;

    StorageManager(AuRawActivity activity, Callbacks callbacks) {
        this.activity = activity;
        this.callbacks = callbacks;
    }

    Intent createRawDocumentPickerIntent() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("*/*");
        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        Uri initialUri = rememberedPickerUri(RAW_PICKER_URI_KEY);
        if (initialUri != null) {
            intent.putExtra(DocumentsContract.EXTRA_INITIAL_URI, initialUri);
        }
        return intent;
    }

    void handleRawDocumentResult(int resultCode, Intent data) {
        if (resultCode != AuRawActivity.RESULT_OK || data == null) {
            callbacks.onFilePicked("", "", "", "", false);
            return;
        }
        ArrayList<Uri> uris = selectedDocumentUris(data);
        if (uris.isEmpty()) {
            callbacks.onFilePicked("", "", "", "", false);
            return;
        }
        new Thread(
                () -> importDocuments(uris),
                uris.size() == 1 ? "AuRaw document import" : "AuRaw document batch import")
                .start();
    }

    private Uri rememberedPickerUri(String key) {
        String uriText = activity
                .getSharedPreferences(PICKER_PREFERENCES, AuRawActivity.MODE_PRIVATE)
                .getString(key, "");
        if (uriText == null || uriText.isEmpty()) {
            return null;
        }
        try {
            Uri uri = Uri.parse(uriText);
            return ContentResolver.SCHEME_CONTENT.equals(uri.getScheme()) ? uri : null;
        } catch (Exception ignored) {
            return null;
        }
    }

    private void rememberPickerUri(String key, Uri uri) {
        if (uri == null || !ContentResolver.SCHEME_CONTENT.equals(uri.getScheme())) {
            return;
        }
        activity.getSharedPreferences(PICKER_PREFERENCES, AuRawActivity.MODE_PRIVATE)
                .edit()
                .putString(key, uri.toString())
                .apply();
    }

    void scavengeTemporaryRawFiles() {
        File[] cachedFiles = activity.getCacheDir().listFiles((directory, name) ->
                name.startsWith("auraw-library-")
                        || name.startsWith("auraw-import-")
                        || name.startsWith("auraw-sidecar-")
                        || name.startsWith("auraw-thumbnail-"));
        deleteStaleFiles(cachedFiles);

        File[] partialImports = rawLibraryDirectory().listFiles((directory, name) ->
                (name.startsWith(".auraw-import-") || name.startsWith(".auraw-sidecar-"))
                        && name.endsWith(".part"));
        deleteStaleFiles(partialImports);
    }

    private static void deleteStaleFiles(File[] files) {
        if (files == null) {
            return;
        }
        long now = System.currentTimeMillis();
        for (File file : files) {
            long modified = file.lastModified();
            boolean isStale = modified > 0L
                    && now >= modified
                    && now - modified >= STALE_TEMP_FILE_AGE_MS;
            if (file.isFile() && isStale && !file.delete() && file.exists()) {
                file.deleteOnExit();
            }
        }
    }

    private static ArrayList<Uri> selectedDocumentUris(Intent data) {
        ArrayList<Uri> uris = new ArrayList<>();
        HashSet<String> seen = new HashSet<>();
        ClipData clipData = data.getClipData();
        if (clipData != null) {
            for (int index = 0; index < clipData.getItemCount(); index++) {
                Uri uri = clipData.getItemAt(index).getUri();
                if (uri != null && seen.add(uri.toString())) {
                    uris.add(uri);
                }
            }
        }
        Uri single = data.getData();
        if (single != null && seen.add(single.toString())) {
            uris.add(single);
        }
        return uris;
    }

    private void importDocuments(ArrayList<Uri> uris) {
        if (uris.size() == 1) {
            Uri uri = uris.get(0);
            importSingleDocument(uri, queryDisplayName(uri));
            return;
        }

        int imported = 0;
        int failed = 0;
        ArrayList<String> errors = new ArrayList<>();
        for (Uri uri : uris) {
            String displayName = queryDisplayName(uri);
            StoredRaw stored = null;
            try {
                stored = importDocumentIntoLibrary(uri, displayName);
                if (imported == 0) {
                    rememberPickerUri(RAW_PICKER_URI_KEY, uri);
                }
                imported++;
            } catch (Exception error) {
                if (stored != null) {
                    deleteStoredRaw(stored.uri);
                }
                failed++;
                if (errors.size() < 4) {
                    errors.add(displayName + ": " + error);
                }
            }
        }
        if (failed > errors.size()) {
            errors.add((failed - errors.size()) + " additional import(s) failed");
        }
        callbacks.onImportBatchFinished(imported, failed, String.join("\n", errors));
    }

    private void importSingleDocument(Uri uri, String displayName) {
        StoredRaw stored = null;
        try {
            stored = importDocumentIntoLibrary(uri, displayName);
            rememberPickerUri(RAW_PICKER_URI_KEY, uri);
            deliverLibraryRawFd(stored.uri, stored.displayName);
        } catch (Exception error) {
            if (stored != null) {
                deleteStoredRaw(stored.uri);
            }
            callbacks.onFilePicked("", displayName, "", error.toString(), false);
        }
    }

    private StoredRaw importDocumentIntoLibrary(Uri uri, String displayName) throws Exception {
        if (!isRawName(displayName)) {
            throw new IllegalArgumentException(
                    "Choose a supported RAW file (for example DNG, CR3, NEF, ARW, RAF, or RW2)");
        }
        Long declaredSize = queryDocumentSize(uri);
        if (declaredSize != null && declaredSize > MAX_RAW_IMPORT_BYTES) {
            throw new IllegalStateException(
                    "The selected RAW is " + declaredSize
                            + " bytes; the Android import limit is "
                            + MAX_RAW_IMPORT_BYTES);
        }
        return storeRawInLibrary(uri, displayName);
    }

    /** Human-readable storage location shown by the Rust library UI. */
    String rawLibraryLocation() {
        return rawLibraryDirectory().getAbsolutePath();
    }

    /** Lists the canonical .library plus any not-yet-migrated upgrade entries. */
    String listRawLibrary() {
        try {
            return listCombinedRawLibrary();
        } catch (Exception error) {
            throw new IllegalStateException("Could not list the RAW library", error);
        }
    }

    /** Returns an owned native descriptor; Rust closes it after thumbnail extraction. */
    int openRawLibraryFd(String uriText) throws Exception {
        Uri uri = Uri.parse(uriText);
        ParcelFileDescriptor descriptor;
        if (ContentResolver.SCHEME_FILE.equals(uri.getScheme())) {
            descriptor = ParcelFileDescriptor.open(
                    new File(uri.getPath()), ParcelFileDescriptor.MODE_READ_ONLY);
        } else {
            descriptor = activity.getContentResolver().openFileDescriptor(uri, "r");
        }
        if (descriptor == null) {
            throw new IllegalStateException("The RAW library returned no file descriptor");
        }
        return descriptor.detachFd();
    }

    /**
     * Returns a persistent private JPEG path for an unedited RAW thumbnail. The
     * RAW identity is part of the key, so replacing a MediaStore item never
     * reuses pixels from the older file.
     */
    String rawThumbnailCachePath(
            String uriText,
            long bytes,
            long modifiedSeconds,
            int maximumEdge) throws Exception {
        return thumbnailCachePath(
                "raw\n" + uriText + "\n" + bytes + "\n" + modifiedSeconds
                        + "\n" + maximumEdge,
                ".raw.jpg").getAbsolutePath();
    }

    /** Edited thumbnails are validated by Rust against the exact sidecar bytes. */
    String developedThumbnailCachePath(String uriText) throws Exception {
        return thumbnailCachePath("developed\n" + uriText, ".developed.jpg").getAbsolutePath();
    }

    /** Clears regenerable RAW and edited library previews from both cache generations. */
    void clearThumbnailCache() {
        clearThumbnailCacheDirectory(persistentThumbnailCacheDirectory());
        clearThumbnailCacheDirectory(
                new File(activity.getCacheDir(), "library-thumbnails"));
    }

    /**
     * Slow compatibility fallback for providers/LibRaw builds that cannot seek
     * through /proc/self/fd. The first successful decode is cached as JPEG, so
     * this full RAW copy is not repeated when the library is reopened.
     */
    String materializeRawLibraryThumbnail(String uriText, String displayName)
            throws Exception {
        Uri uri = Uri.parse(uriText);
        verifyRawLibraryIdentity(uri, displayName);
        String safeName = safeRawName(displayName);
        int dot = safeName.lastIndexOf('.');
        String suffix = dot >= 0 ? safeName.substring(dot) : ".raw";
        File cached = File.createTempFile("auraw-thumbnail-", suffix, activity.getCacheDir());
        boolean completed = false;
        try {
            try (InputStream input = openLibraryInput(uri);
                 FileOutputStream output = new FileOutputStream(cached)) {
                if (input == null) {
                    throw new IllegalStateException("Android storage returned no RAW stream");
                }
                copy(input, output, MAX_RAW_IMPORT_BYTES);
                output.getFD().sync();
            }
            completed = true;
            return cached.getAbsolutePath();
        } finally {
            if (!completed && !cached.delete() && cached.exists()) {
                cached.deleteOnExit();
            }
        }
    }

    /** Called when a library thumbnail is selected in Rust. */
    void openRawLibraryDocument(String uriText, String displayName) {
        new Thread(
                () -> {
                    try {
                        deliverLibraryRawFd(Uri.parse(uriText), displayName);
                    } catch (Exception error) {
                        callbacks.onFilePickedFd(-1, displayName, uriText, error.toString());
                    }
                },
                "AuRaw library open").start();
    }

    /** Removes the visible edit sidecar belonging to one library RAW. */
    void removeRawSidecar(String rawUriText, String displayName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        if (!ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())) {
            removeRawSidecarLegacyMediaStore(displayName);
            return;
        }
        File sidecar = new File(
                new File(rawUri.getPath()).getParentFile(), sidecarDisplayName(displayName));
        if (sidecar.exists() && !sidecar.delete()) {
            throw new IllegalStateException("Could not delete the RAW sidecar");
        }
    }

    private void removeRawSidecarLegacyMediaStore(String rawDisplayName) {
        boolean deletionFailed = false;
        for (Uri generation : legacyMediaStoreSidecarUris(rawDisplayName)) {
            deletionFailed |= deleteLegacyMediaStoreSidecarGeneration(generation, rawDisplayName) <= 0;
        }
        if (deletionFailed && !legacyMediaStoreSidecarUris(rawDisplayName).isEmpty()) {
            throw new IllegalStateException("Android storage could not delete the RAW sidecar");
        }
    }

    /** Duplicates a library RAW and its current sidecar into AuRaw's library. */
    String duplicateRawLibraryDocument(String rawUriText, String displayName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        StoredRaw duplicate = null;
        String cachedSidecar = "";
        try {
            duplicate = storeRawInLibrary(rawUri, displayName);
            cachedSidecar = materializeRawSidecar(rawUriText, displayName);
            if (cachedSidecar != null && !cachedSidecar.isEmpty()) {
                publishRawSidecar(cachedSidecar, duplicate.uri.toString(), duplicate.displayName);
            }
            return duplicate.displayName;
        } catch (Exception error) {
            if (duplicate != null) {
                try {
                    deleteStoredRaw(duplicate.uri);
                } catch (Exception ignored) {
                    // Preserve the original failure; best-effort cleanup only.
                }
            }
            throw error;
        } finally {
            if (cachedSidecar != null && !cachedSidecar.isEmpty()) {
                File staged = new File(cachedSidecar);
                if (!staged.delete() && staged.exists()) {
                    staged.deleteOnExit();
                }
            }
        }
    }

    /** Deletes a library RAW and its sidecar after validating AuRaw ownership. */
    void deleteRawLibraryDocument(String rawUriText, String displayName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        removeRawSidecar(rawUriText, displayName);
        if (ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())) {
            File raw = new File(rawUri.getPath());
            if (raw.exists() && !raw.delete()) {
                throw new IllegalStateException("Could not delete the RAW file");
            }
        } else if (activity.getContentResolver().delete(rawUri, null, null) <= 0) {
            throw new IllegalStateException("Android storage could not delete the RAW file");
        }
    }

    /**
     * Copies an existing visible sibling sidecar into private cache. Rust calls
     * this only from its decode worker, then removes the returned cache file.
     * An empty result means that the RAW has no sidecar yet.
     */
    String materializeRawSidecar(String rawUriText, String displayName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        if (!ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())) {
            return materializeRawSidecarLegacyMediaStore(displayName);
        }
        File sidecar = new File(
                new File(rawUri.getPath()).getParentFile(), sidecarDisplayName(displayName));
        if (!sidecar.isFile()) {
            return "";
        }

        File cached = File.createTempFile("auraw-sidecar-", ".auraw", activity.getCacheDir());
        boolean completed = false;
        try {
            try (FileInputStream input = new FileInputStream(sidecar);
                 FileOutputStream output = new FileOutputStream(cached)) {
                copy(input, output, MAX_SIDECAR_BYTES);
                output.getFD().sync();
            }
            completed = true;
            return cached.getAbsolutePath();
        } finally {
            if (!completed && !cached.delete() && cached.exists()) {
                cached.deleteOnExit();
            }
        }
    }

    private String materializeRawSidecarLegacyMediaStore(String rawDisplayName) throws Exception {
        ArrayList<Uri> generations = legacyMediaStoreSidecarUris(rawDisplayName);
        if (generations.isEmpty()) {
            return "";
        }

        Uri newestGeneration = generations.get(0);
        File cached = File.createTempFile("auraw-sidecar-", ".auraw", activity.getCacheDir());
        boolean completed = false;
        try {
            try (InputStream input = activity.getContentResolver().openInputStream(newestGeneration);
                 FileOutputStream output = new FileOutputStream(cached)) {
                if (input == null) {
                    throw new IllegalStateException("Android storage returned no sidecar stream");
                }
                copy(input, output, MAX_SIDECAR_BYTES);
                output.getFD().sync();
            }
            completed = true;
            return cached.getAbsolutePath();
        } finally {
            if (!completed && !cached.delete() && cached.exists()) {
                cached.deleteOnExit();
            }
        }
    }

    /** Creates the private staging file populated by Rust's sidecar worker. */
    String createRawSidecarCache() throws Exception {
        return File.createTempFile("auraw-sidecar-", ".auraw", activity.getCacheDir()).getAbsolutePath();
    }

    /**
     * Publishes a completed staging file beside its RAW. New libraries use a
     * same-directory temporary file and atomic replacement; old MediaStore rows
     * remain supported only until their one-time migration succeeds.
     */
    String publishRawSidecar(
            String cachedPath,
            String rawUriText,
            String displayName) throws Exception {
        File cached = new File(cachedPath);
        if (!cached.isFile() || cached.length() > MAX_SIDECAR_BYTES) {
            throw new IllegalStateException("AuRaw sidecar staging file is missing or too large");
        }
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        if (!ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())) {
            return publishRawSidecarLegacyMediaStore(cached, displayName);
        }
        return publishRawSidecarFile(
                cached, new File(rawUri.getPath()).getParentFile(), displayName);
    }

    private String publishRawSidecarLegacyMediaStore(File cached, String rawDisplayName) throws Exception {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Legacy MediaStore sidecars require Android 10 or newer");
        }
        ContentResolver resolver = activity.getContentResolver();
        String displayName = sidecarDisplayName(rawDisplayName);
        String stagedName = sidecarStagePrefix(rawDisplayName)
                + Long.toUnsignedString(System.nanoTime());
        ArrayList<Uri> oldSidecars = legacyMediaStoreSidecarUris(rawDisplayName);
        ContentValues values = new ContentValues();
        values.put(MediaStore.Downloads.DISPLAY_NAME, stagedName);
        // MediaProvider may rewrite unknown extensions to match a specific
        // MIME type (for example `.auraw.json`). The unknown binary MIME keeps
        // AuRaw's exact custom filename intact.
        values.put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream");
        values.put(MediaStore.Downloads.RELATIVE_PATH, LEGACY_MEDIASTORE_RAW_RELATIVE_PATH);
        values.put(MediaStore.Downloads.IS_PENDING, 1);
        Uri destination = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values);
        if (destination == null) {
            throw new IllegalStateException("Android MediaStore could not create the sidecar");
        }

        boolean contentPublished = false;
        try {
            try (FileInputStream input = new FileInputStream(cached);
                 OutputStream output = resolver.openOutputStream(destination, "w")) {
                if (output == null) {
                    throw new IllegalStateException("Android storage returned no sidecar output");
                }
                copy(input, output, MAX_SIDECAR_BYTES);
                output.flush();
            }
            values.clear();
            values.put(MediaStore.Downloads.IS_PENDING, 0);
            if (resolver.update(destination, values, null, null) <= 0) {
                throw new IllegalStateException("Android MediaStore could not publish the sidecar");
            }
            contentPublished = true;
            boolean removedOldRows = true;
            for (Uri oldSidecar : oldSidecars) {
                if (!oldSidecar.equals(destination)) {
                    removedOldRows &= deleteLegacyMediaStoreSidecarGeneration(
                            oldSidecar, rawDisplayName) > 0;
                }
            }
            if (!removedOldRows) {
                return LEGACY_MEDIASTORE_RAW_RELATIVE_PATH + stagedName;
            }
            values.clear();
            values.put(MediaStore.Downloads.DISPLAY_NAME, displayName);
            if (resolver.update(destination, values, null, null) <= 0) {
                return LEGACY_MEDIASTORE_RAW_RELATIVE_PATH + stagedName;
            }
            String actualName = queryStoredDisplayName(destination);
            if (!displayName.equals(actualName)) {
                values.clear();
                values.put(MediaStore.Downloads.DISPLAY_NAME, stagedName);
                resolver.update(destination, values, null, null);
                return LEGACY_MEDIASTORE_RAW_RELATIVE_PATH + queryStoredDisplayName(destination);
            }
            return LEGACY_MEDIASTORE_RAW_RELATIVE_PATH + displayName;
        } finally {
            // Once the staged row is published it is a complete, discoverable
            // recovery generation. Preserve it if final renaming fails.
            if (!contentPublished) {
                resolver.delete(destination, null, null);
            }
        }
    }

    private String publishRawSidecarFile(
            File cached,
            File directory,
            String rawDisplayName) throws Exception {
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create " + directory);
        }
        File destination = new File(directory, sidecarDisplayName(rawDisplayName));
        File temporary = File.createTempFile(".auraw-sidecar-", ".part", directory);
        boolean published = false;
        try {
            try (FileInputStream input = new FileInputStream(cached);
                 FileOutputStream output = new FileOutputStream(temporary)) {
                copy(input, output, MAX_SIDECAR_BYTES);
                output.getFD().sync();
            }
            try {
                Files.move(
                        temporary.toPath(),
                        destination.toPath(),
                        StandardCopyOption.ATOMIC_MOVE,
                        StandardCopyOption.REPLACE_EXISTING);
            } catch (AtomicMoveNotSupportedException unsupported) {
                // Some removable/external filesystems do not advertise atomic
                // moves. This remains a same-directory replacement and avoids
                // making persistence unavailable on those devices.
                Files.move(
                        temporary.toPath(),
                        destination.toPath(),
                        StandardCopyOption.REPLACE_EXISTING);
            }
            published = true;
            return destination.getAbsolutePath();
        } finally {
            if (!published && !temporary.delete() && temporary.exists()) {
                temporary.deleteOnExit();
            }
        }
    }

    private ArrayList<Uri> legacyMediaStoreSidecarUris(String rawDisplayName) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Legacy MediaStore sidecars require Android 10 or newer");
        }
        ArrayList<Uri> result = new ArrayList<>();
        String displayName = sidecarDisplayName(rawDisplayName);
        String stagedPrefix = sidecarStagePrefix(rawDisplayName);
        String[] projection = {
                MediaStore.Downloads._ID,
                MediaStore.Downloads.DISPLAY_NAME
        };
        String selection = legacyMediaStoreSidecarSelection();
        String[] args = legacyMediaStoreSidecarSelectionArgs(displayName, stagedPrefix);
        try (Cursor cursor = activity.getContentResolver().query(
                MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                projection,
                selection,
                args,
                MediaStore.Downloads._ID + " DESC")) {
            if (cursor == null) {
                throw new IllegalStateException("Android MediaStore returned no sidecar cursor");
            }
            int idColumn = cursor.getColumnIndexOrThrow(MediaStore.Downloads._ID);
            int nameColumn = cursor.getColumnIndexOrThrow(MediaStore.Downloads.DISPLAY_NAME);
            while (cursor.moveToNext()) {
                String foundName = cursor.getString(nameColumn);
                if (foundName != null
                        && (displayName.equals(foundName) || foundName.startsWith(stagedPrefix))) {
                    result.add(ContentUris.withAppendedId(
                            MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                            cursor.getLong(idColumn)));
                }
            }
        }
        return result;
    }

    private int deleteLegacyMediaStoreSidecarGeneration(Uri generation, String rawDisplayName) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Legacy MediaStore sidecars require Android 10 or newer");
        }
        long generationId = ContentUris.parseId(generation);
        String displayName = sidecarDisplayName(rawDisplayName);
        String stagedPrefix = sidecarStagePrefix(rawDisplayName);
        String selection = MediaStore.Downloads._ID + "=? AND ("
                + legacyMediaStoreSidecarSelection() + ")";
        String[] sidecarArgs = legacyMediaStoreSidecarSelectionArgs(displayName, stagedPrefix);
        String[] args = new String[sidecarArgs.length + 1];
        args[0] = Long.toString(generationId);
        System.arraycopy(sidecarArgs, 0, args, 1, sidecarArgs.length);
        return activity.getContentResolver().delete(
                MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                selection,
                args);
    }

    private static String legacyMediaStoreSidecarSelection() {
        return MediaStore.Downloads.RELATIVE_PATH + "=? AND "
                + MediaStore.Downloads.OWNER_PACKAGE_NAME + "=? AND "
                + MediaStore.Downloads.IS_PENDING + "=0 AND ("
                + MediaStore.Downloads.DISPLAY_NAME + "=? OR "
                + MediaStore.Downloads.DISPLAY_NAME + " LIKE ? ESCAPE '\\')";
    }

    private String[] legacyMediaStoreSidecarSelectionArgs(String displayName, String stagedPrefix) {
        return new String[]{
                LEGACY_MEDIASTORE_RAW_RELATIVE_PATH,
                activity.getPackageName(),
                displayName,
                escapeLike(stagedPrefix) + "%"
        };
    }

    private void verifyRawLibraryIdentity(Uri rawUri, String expectedDisplayName) throws Exception {
        if (ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())) {
            verifyFileRawLibraryIdentity(rawUri, expectedDisplayName);
            return;
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            verifyLegacyMediaStoreRawIdentity(rawUri, expectedDisplayName);
            return;
        }
        throw new IllegalArgumentException("The RAW library URI is invalid");
    }

    private void verifyFileRawLibraryIdentity(
            Uri rawUri,
            String expectedDisplayName) throws Exception {
        if (rawUri.getPath() == null || expectedDisplayName == null) {
            throw new IllegalArgumentException("The RAW library URI is invalid");
        }
        File raw = new File(rawUri.getPath()).getCanonicalFile();
        File parent = raw.getParentFile();
        File library = rawLibraryDirectory().getCanonicalFile();
        File legacyRoot = externalMediaRootDirectory().getCanonicalFile();
        if (!expectedDisplayName.equals(raw.getName())
                || parent == null
                || (!library.equals(parent) && !legacyRoot.equals(parent))) {
            throw new IllegalArgumentException("The RAW is outside AuRaw's library");
        }
    }

    private void verifyLegacyMediaStoreRawIdentity(
            Uri rawUri,
            String expectedDisplayName) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Legacy MediaStore RAW storage requires Android 10 or newer");
        }
        Uri collection = MediaStore.Downloads.EXTERNAL_CONTENT_URI;
        if (expectedDisplayName == null
                || !isRawName(expectedDisplayName)
                || !ContentResolver.SCHEME_CONTENT.equals(rawUri.getScheme())
                || !collection.getAuthority().equals(rawUri.getAuthority())) {
            throw new IllegalArgumentException("The RAW library URI is invalid");
        }

        long expectedId;
        try {
            expectedId = ContentUris.parseId(rawUri);
        } catch (NumberFormatException error) {
            throw new IllegalArgumentException("The RAW library URI is invalid", error);
        }
        if (expectedId < 0
                || !ContentUris.withAppendedId(collection, expectedId).equals(rawUri)) {
            throw new IllegalArgumentException("The RAW library URI is invalid");
        }

        String[] projection = {
                MediaStore.Downloads._ID,
                MediaStore.Downloads.DISPLAY_NAME,
                MediaStore.Downloads.RELATIVE_PATH,
                MediaStore.Downloads.OWNER_PACKAGE_NAME,
                MediaStore.Downloads.IS_PENDING
        };
        try (Cursor cursor = activity.getContentResolver().query(
                rawUri,
                projection,
                null,
                null,
                null)) {
            if (cursor == null || !cursor.moveToFirst()) {
                throw new IllegalArgumentException("The RAW is not in AuRaw's library");
            }
            long storedId = cursor.getLong(cursor.getColumnIndexOrThrow(
                    MediaStore.Downloads._ID));
            String storedName = cursor.getString(cursor.getColumnIndexOrThrow(
                    MediaStore.Downloads.DISPLAY_NAME));
            String storedPath = cursor.getString(cursor.getColumnIndexOrThrow(
                    MediaStore.Downloads.RELATIVE_PATH));
            String storedOwner = cursor.getString(cursor.getColumnIndexOrThrow(
                    MediaStore.Downloads.OWNER_PACKAGE_NAME));
            int pending = cursor.getInt(cursor.getColumnIndexOrThrow(
                    MediaStore.Downloads.IS_PENDING));
            if (storedId != expectedId
                    || !expectedDisplayName.equals(storedName)
                    || !LEGACY_MEDIASTORE_RAW_RELATIVE_PATH.equals(storedPath)
                    || !activity.getPackageName().equals(storedOwner)
                    || pending != 0
                    || cursor.moveToNext()) {
                throw new IllegalArgumentException("The RAW is outside AuRaw's library");
            }
        }
    }

    private static String sidecarDisplayName(String rawDisplayName) {
        String name = safeRawName(rawDisplayName);
        if (!name.equals(rawDisplayName)
                || name.getBytes(StandardCharsets.UTF_8).length > 240) {
            throw new IllegalArgumentException("The RAW name cannot be used for a sidecar");
        }
        return name + ".auraw";
    }

    private File thumbnailCachePath(String identity, String suffix) throws Exception {
        File directory = persistentThumbnailCacheDirectory();
        byte[] digest = MessageDigest.getInstance("SHA-256").digest(
                identity.getBytes(StandardCharsets.UTF_8));
        StringBuilder name = new StringBuilder();
        for (byte value : digest) {
            name.append(String.format(Locale.ROOT, "%02x", value & 0xff));
        }
        File cached = new File(directory, name.append(suffix).toString());
        migrateLegacyThumbnailCacheEntry(cached);
        touchThumbnailCacheEntry(cached);
        trimThumbnailCache(directory);
        return cached;
    }

    /**
     * Thumbnail JPEGs are regenerable, but keeping them in no-backup app storage
     * prevents Android's cache scavenger from discarding the whole library
     * between launches. They still disappear when app data is cleared or the
     * app is uninstalled, and the bounded LRU below prevents unbounded growth.
     */
    private File persistentThumbnailCacheDirectory() {
        File directory = new File(activity.getNoBackupFilesDir(), "library-thumbnails");
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create the persistent thumbnail cache");
        }
        return directory;
    }

    private static void clearThumbnailCacheDirectory(File directory) {
        if (!directory.exists()) {
            return;
        }
        File[] entries = directory.listFiles();
        if (entries == null) {
            throw new IllegalStateException(
                    "Could not inspect thumbnail cache " + directory);
        }
        for (File entry : entries) {
            if (!entry.isFile() || (!entry.delete() && entry.exists())) {
                throw new IllegalStateException(
                        "Could not clear thumbnail cache entry " + entry);
            }
        }
    }

    /** Lazily preserves cache entries written by releases that used getCacheDir(). */
    private void migrateLegacyThumbnailCacheEntry(File destination) {
        File legacyDirectory = new File(activity.getCacheDir(), "library-thumbnails");
        if (!legacyDirectory.isDirectory()) {
            return;
        }
        migrateLegacyThumbnailCacheFile(
                new File(legacyDirectory, destination.getName()), destination);
        File destinationFingerprint = new File(destination.getPath() + ".fingerprint");
        migrateLegacyThumbnailCacheFile(
                new File(legacyDirectory, destination.getName() + ".fingerprint"),
                destinationFingerprint);
    }

    private static void migrateLegacyThumbnailCacheFile(File source, File destination) {
        if (destination.isFile() || !source.isFile()) {
            return;
        }
        try {
            moveOrCopyLegacyFile(source, destination, MAX_THUMBNAIL_CACHE_BYTES);
        } catch (Exception error) {
            Log.w(LOG_TAG, "Could not migrate legacy thumbnail cache entry", error);
        }
    }

    private static void touchThumbnailCacheEntry(File cached) {
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

    private static void trimThumbnailCache(File directory) {
        // PNG cache entries from older builds are intentionally discarded, not decoded.
        File[] legacyPngs = directory.listFiles(
                (parent, name) -> name.endsWith(".png")
                        || name.endsWith(".png.fingerprint"));
        if (legacyPngs != null) {
            for (File legacyPng : legacyPngs) {
                deleteCacheFile(legacyPng);
            }
        }

        File[] thumbnails = directory.listFiles((parent, name) -> name.endsWith(".jpg"));
        if (thumbnails != null && thumbnails.length > MAX_THUMBNAIL_CACHE_ENTRIES) {
            Arrays.sort(
                    thumbnails,
                    (left, right) -> Long.compare(left.lastModified(), right.lastModified()));
            int remove = thumbnails.length - MAX_THUMBNAIL_CACHE_ENTRIES;
            for (int index = 0; index < remove; index++) {
                deleteThumbnailCacheEntry(thumbnails[index]);
            }
        }

        // A crash between the JPEG and fingerprint writes may leave an orphan.
        File[] fingerprints = directory.listFiles(
                (parent, name) -> name.endsWith(".developed.jpg.fingerprint"));
        if (fingerprints == null) {
            return;
        }
        for (File fingerprint : fingerprints) {
            String name = fingerprint.getName();
            String thumbnailName = name.substring(0, name.length() - ".fingerprint".length());
            if (!new File(directory, thumbnailName).isFile()) {
                deleteCacheFile(fingerprint);
            }
        }
    }

    private static void deleteThumbnailCacheEntry(File thumbnail) {
        deleteCacheFile(thumbnail);
        deleteCacheFile(new File(thumbnail.getPath() + ".fingerprint"));
    }

    private static void deleteCacheFile(File file) {
        if (!file.delete() && file.exists()) {
            file.deleteOnExit();
        }
    }

    private static String sidecarStagePrefix(String rawDisplayName) {
        try {
            byte[] digest = MessageDigest.getInstance("SHA-256").digest(
                    rawDisplayName.getBytes(StandardCharsets.UTF_8));
            StringBuilder prefix = new StringBuilder(".auraw-stage-");
            for (int index = 0; index < 16; index++) {
                prefix.append(String.format(Locale.ROOT, "%02x", digest[index] & 0xff));
            }
            return prefix.append('-').toString();
        } catch (Exception impossible) {
            // SHA-256 is mandatory on Android. Keep a deterministic fallback
            // so a vendor provider cannot disable sidecar saving entirely.
            return ".auraw-stage-"
                    + Integer.toUnsignedString(rawDisplayName.hashCode(), 16) + '-';
        }
    }

    private static String escapeLike(String value) {
        return value.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_");
    }

    private StoredRaw storeRawInLibrary(Uri source, String requestedName) throws Exception {
        return storeRawFile(source, requestedName);
    }

    private StoredRaw storeRawFile(Uri source, String requestedName) throws Exception {
        File directory = rawLibraryDirectory();
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create " + directory);
        }
        File destination = uniqueRawFile(directory, safeRawName(requestedName));
        File partial = uniqueRawFile(
                directory,
                ".auraw-import-" + destination.getName() + ".part");
        boolean completed = false;
        try {
            try (InputStream input = openLibraryInput(source);
                 OutputStream output = new FileOutputStream(partial)) {
                if (input == null) {
                    throw new IllegalStateException("The document provider returned no input stream");
                }
                copy(input, output, MAX_RAW_IMPORT_BYTES);
            }
            if (!partial.renameTo(destination)) {
                throw new IllegalStateException("Could not publish the imported RAW in " + directory);
            }
            completed = true;
            return new StoredRaw(Uri.fromFile(destination), destination.getName());
        } finally {
            if (!completed && !partial.delete() && partial.exists()) {
                partial.deleteOnExit();
            }
        }
    }

    private void deliverLibraryRawFd(Uri source, String displayName) throws Exception {
        verifyRawLibraryIdentity(source, displayName);
        int fd = openRawLibraryFd(source.toString());
        boolean handedOff = false;
        try {
            callbacks.onFilePickedFd(fd, displayName, source.toString(), "");
            handedOff = true;
        } finally {
            if (!handedOff) {
                try {
                    ParcelFileDescriptor.adoptFd(fd).close();
                } catch (Exception ignored) {
                    // The native side owns the descriptor only after a successful handoff.
                }
            }
        }
    }

    private InputStream openLibraryInput(Uri uri) throws Exception {
        if (ContentResolver.SCHEME_FILE.equals(uri.getScheme())) {
            return new FileInputStream(new File(uri.getPath()));
        }
        return activity.getContentResolver().openInputStream(uri);
    }

    private String listCombinedRawLibrary() {
        ArrayList<RawLibraryRecord> records = new ArrayList<>();
        records.addAll(listFileRawLibrary(rawLibraryDirectory()));
        try {
            records.addAll(listFileRawLibrary(externalMediaRootDirectory()));
        } catch (IllegalStateException error) {
            Log.w(LOG_TAG, "Could not inspect the pre-.library RAW location", error);
        }
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            try {
                records.addAll(listLegacyMediaStoreRawLibrary());
            } catch (IllegalStateException error) {
                Log.w(LOG_TAG, "Could not inspect the legacy MediaStore RAW library", error);
            }
        }
        records.sort((left, right) -> {
            int modifiedOrder = Long.compare(right.modifiedSeconds, left.modifiedSeconds);
            return modifiedOrder != 0 ? modifiedOrder : left.uri.compareTo(right.uri);
        });

        StringBuilder result = new StringBuilder();
        Set<String> seenUris = new HashSet<>();
        int added = 0;
        for (RawLibraryRecord record : records) {
            if (!seenUris.add(record.uri)) {
                continue;
            }
            if (added > MAX_RAW_LIBRARY_FILES) {
                break;
            }
            appendLibraryRecord(
                    result,
                    record.uri,
                    record.displayName,
                    record.displayPath,
                    record.bytes,
                    record.modifiedSeconds);
            added++;
        }
        return result.toString();
    }

    private ArrayList<RawLibraryRecord> listLegacyMediaStoreRawLibrary() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Legacy MediaStore RAW storage requires Android 10 or newer");
        }
        ArrayList<RawLibraryRecord> result = new ArrayList<>();
        String[] projection = {
                MediaStore.Downloads._ID,
                MediaStore.Downloads.DISPLAY_NAME,
                MediaStore.Downloads.SIZE,
                MediaStore.Downloads.DATE_MODIFIED
        };
        String selection = MediaStore.Downloads.RELATIVE_PATH + "=? AND "
                + MediaStore.Downloads.OWNER_PACKAGE_NAME + "=? AND "
                + MediaStore.Downloads.IS_PENDING + "=0";
        String[] selectionArgs = {LEGACY_MEDIASTORE_RAW_RELATIVE_PATH, activity.getPackageName()};
        try (Cursor cursor = activity.getContentResolver().query(
                MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                projection,
                selection,
                selectionArgs,
                MediaStore.Downloads.DATE_MODIFIED + " DESC")) {
            if (cursor == null) {
                throw new IllegalStateException("Android MediaStore returned no RAW cursor");
            }
            int idColumn = cursor.getColumnIndexOrThrow(MediaStore.Downloads._ID);
            int nameColumn = cursor.getColumnIndexOrThrow(MediaStore.Downloads.DISPLAY_NAME);
            int sizeColumn = cursor.getColumnIndexOrThrow(MediaStore.Downloads.SIZE);
            int modifiedColumn = cursor.getColumnIndexOrThrow(MediaStore.Downloads.DATE_MODIFIED);
            // Return one sentinel record beyond the UI limit so Rust can
            // distinguish exactly 20,000 files from a truncated collection.
            while (result.size() <= MAX_RAW_LIBRARY_FILES && cursor.moveToNext()) {
                String name = cursor.getString(nameColumn);
                if (!isRawName(name)) {
                    continue;
                }
                Uri uri = ContentUris.withAppendedId(
                        MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                        cursor.getLong(idColumn));
                result.add(new RawLibraryRecord(
                        uri.toString(),
                        name,
                        LEGACY_MEDIASTORE_RAW_RELATIVE_PATH + name,
                        Math.max(0, cursor.getLong(sizeColumn)),
                        Math.max(0, cursor.getLong(modifiedColumn))));
            }
        }
        return result;
    }

    private ArrayList<RawLibraryRecord> listFileRawLibrary(File directory) {
        ArrayList<RawLibraryRecord> result = new ArrayList<>();
        File[] files = directory.listFiles();
        if (files == null) {
            return result;
        }
        Arrays.sort(files, (left, right) -> Long.compare(right.lastModified(), left.lastModified()));
        for (File file : files) {
            // Return one sentinel record beyond the UI limit; Rust displays
            // only the first MAX_RAW_LIBRARY_FILES entries.
            if (result.size() > MAX_RAW_LIBRARY_FILES) {
                break;
            }
            if (!file.isFile() || !isRawName(file.getName())) {
                continue;
            }
            result.add(new RawLibraryRecord(
                    Uri.fromFile(file).toString(),
                    file.getName(),
                    file.getAbsolutePath(),
                    Math.max(0, file.length()),
                    Math.max(0, file.lastModified() / 1000)));
        }
        return result;
    }

    private static void appendLibraryRecord(
            StringBuilder result,
            String uri,
            String displayName,
            String displayPath,
            long bytes,
            long modifiedSeconds) {
        result.append(Uri.encode(uri)).append('\t')
                .append(Uri.encode(displayName)).append('\t')
                .append(Uri.encode(displayPath)).append('\t')
                .append(bytes).append('\t')
                .append(modifiedSeconds).append('\n');
    }

    private String queryStoredDisplayName(Uri uri) {
        String[] projection = {MediaStore.Downloads.DISPLAY_NAME};
        try (Cursor cursor = activity.getContentResolver().query(uri, projection, null, null, null)) {
            if (cursor == null || !cursor.moveToFirst()) {
                throw new IllegalStateException("Android storage returned no stored filename");
            }
            String name = cursor.getString(cursor.getColumnIndexOrThrow(
                    MediaStore.Downloads.DISPLAY_NAME));
            if (name == null || name.isEmpty()) {
                throw new IllegalStateException("Android storage returned an empty filename");
            }
            return name;
        }
    }

    private File externalMediaRootDirectory() {
        File[] mediaDirectories = activity.getExternalMediaDirs();
        if (mediaDirectories != null) {
            for (File directory : mediaDirectories) {
                if (directory != null) {
                    return directory;
                }
            }
        }
        throw new IllegalStateException("Android shared media storage is unavailable");
    }

    private File rawLibraryDirectory() {
        File directory = new File(externalMediaRootDirectory(), RAW_LIBRARY_DIRECTORY_NAME);
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create " + directory);
        }
        File noMedia = new File(directory, ".nomedia");
        try {
            if (!noMedia.exists() && !noMedia.createNewFile()) {
                Log.w(LOG_TAG, "Could not create .nomedia marker in " + directory);
            }
        } catch (Exception error) {
            Log.w(LOG_TAG, "Could not create .nomedia marker in " + directory, error);
        }
        return directory;
    }

    void startLegacyRawStorageMigration() {
        new Thread(() -> {
            try {
                migrateLegacyExternalMediaRoot();
                if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                    migrateLegacyMediaStoreRawLibrary();
                }
            } catch (Exception error) {
                Log.w(LOG_TAG, "Legacy RAW library migration did not complete", error);
            }
        }, "AuRaw RAW library migration").start();
    }

    private void migrateLegacyExternalMediaRoot() {
        File root = externalMediaRootDirectory();
        File library = rawLibraryDirectory();
        File[] files = root.listFiles();
        if (files == null) {
            return;
        }
        for (File source : files) {
            if (!source.isFile() || !isRawName(source.getName())) {
                continue;
            }
            File destination = new File(library, source.getName());
            if (destination.exists()) {
                continue;
            }
            try {
                moveOrCopyLegacyFile(source, destination, MAX_RAW_IMPORT_BYTES);
            } catch (Exception error) {
                Log.w(LOG_TAG, "Could not migrate legacy file RAW " + source.getName(), error);
                continue;
            }

            File sourceSidecar = new File(root, sidecarDisplayName(source.getName()));
            File destinationSidecar = new File(library, sidecarDisplayName(source.getName()));
            if (sourceSidecar.isFile() && !destinationSidecar.exists()) {
                try {
                    moveOrCopyLegacyFile(sourceSidecar, destinationSidecar, MAX_SIDECAR_BYTES);
                } catch (Exception error) {
                    Log.w(LOG_TAG, "Could not migrate legacy file sidecar " + sourceSidecar.getName(), error);
                }
            }
        }

        // Recover a sidecar left behind if a previous run moved its RAW first
        // and then failed while moving the sidecar. Do not attach a sidecar to
        // a same-named collision that still has its original RAW in the root.
        for (File sourceSidecar : files) {
            String sidecarName = sourceSidecar.getName();
            if (!sourceSidecar.isFile() || !sidecarName.endsWith(".auraw")) {
                continue;
            }
            String rawName = sidecarName.substring(0, sidecarName.length() - ".auraw".length());
            if (new File(root, rawName).exists()) {
                continue;
            }
            File destinationRaw = new File(library, rawName);
            File destinationSidecar = new File(library, sidecarName);
            if (!destinationRaw.isFile() || destinationSidecar.exists()) {
                continue;
            }
            try {
                moveOrCopyLegacyFile(sourceSidecar, destinationSidecar, MAX_SIDECAR_BYTES);
            } catch (Exception error) {
                Log.w(LOG_TAG, "Could not recover legacy file sidecar " + sidecarName, error);
            }
        }
    }

    private void migrateLegacyMediaStoreRawLibrary() {
        for (RawLibraryRecord record : listLegacyMediaStoreRawLibrary()) {
            Uri source = Uri.parse(record.uri);
            File destination = new File(rawLibraryDirectory(), safeRawName(record.displayName));
            if (destination.exists()) {
                continue;
            }
            File partial = new File(
                    rawLibraryDirectory(), ".auraw-migrate-" + destination.getName() + ".part");
            String cachedSidecar = "";
            boolean rawPublished = false;
            try {
                try (InputStream input = activity.getContentResolver().openInputStream(source);
                     FileOutputStream output = new FileOutputStream(partial)) {
                    if (input == null) {
                        throw new IllegalStateException("Android storage returned no legacy RAW stream");
                    }
                    copy(input, output, MAX_RAW_IMPORT_BYTES);
                    output.getFD().sync();
                }
                if (destination.exists() || !partial.renameTo(destination)) {
                    throw new IllegalStateException("Could not migrate legacy RAW into " + rawLibraryDirectory());
                }
                rawPublished = true;

                cachedSidecar = materializeRawSidecarLegacyMediaStore(record.displayName);
                if (cachedSidecar != null && !cachedSidecar.isEmpty()) {
                    publishRawSidecarFile(new File(cachedSidecar), rawLibraryDirectory(), destination.getName());
                }

                if (activity.getContentResolver().delete(source, null, null) <= 0) {
                    throw new IllegalStateException(
                            "Could not remove legacy MediaStore RAW after migration: " + source);
                }
                try {
                    removeRawSidecarLegacyMediaStore(record.displayName);
                } catch (Exception cleanupError) {
                    Log.w(LOG_TAG, "Migrated RAW but could not remove every legacy sidecar", cleanupError);
                }
                rawPublished = false;
            } catch (Exception error) {
                Log.w(LOG_TAG, "Could not migrate legacy RAW " + record.displayName, error);
                if (rawPublished) {
                    if (!destination.delete() && destination.exists()) {
                        destination.deleteOnExit();
                    }
                    File destinationSidecar = new File(
                            rawLibraryDirectory(), sidecarDisplayName(destination.getName()));
                    if (!destinationSidecar.delete() && destinationSidecar.exists()) {
                        destinationSidecar.deleteOnExit();
                    }
                }
            } finally {
                if (!partial.delete() && partial.exists()) {
                    partial.deleteOnExit();
                }
                if (cachedSidecar != null && !cachedSidecar.isEmpty()) {
                    File cached = new File(cachedSidecar);
                    if (!cached.delete() && cached.exists()) {
                        cached.deleteOnExit();
                    }
                }
            }
        }
    }

    private static void moveOrCopyLegacyFile(File source, File destination, long maximumBytes)
            throws Exception {
        try {
            Files.move(source.toPath(), destination.toPath(), StandardCopyOption.ATOMIC_MOVE);
            return;
        } catch (AtomicMoveNotSupportedException unsupported) {
            try {
                Files.move(source.toPath(), destination.toPath());
                return;
            } catch (Exception ignored) {
                // Fall through to bounded copy for odd vendor filesystems.
            }
        } catch (Exception ignored) {
            // Fall through to bounded copy for odd vendor filesystems.
        }

        File partial = new File(destination.getParentFile(), ".auraw-move-" + destination.getName() + ".part");
        boolean published = false;
        try {
            try (FileInputStream input = new FileInputStream(source);
                 FileOutputStream output = new FileOutputStream(partial)) {
                copy(input, output, maximumBytes);
                output.getFD().sync();
            }
            if (!partial.renameTo(destination)) {
                throw new IllegalStateException("Could not publish migrated file " + destination);
            }
            published = true;
            if (!source.delete() && source.exists()) {
                if (!destination.delete() && destination.exists()) {
                    destination.deleteOnExit();
                }
                published = false;
                throw new IllegalStateException("Could not remove old file " + source);
            }
        } finally {
            if (!published && !partial.delete() && partial.exists()) {
                partial.deleteOnExit();
            }
        }
    }

    private void deleteStoredRaw(Uri uri) {
        try {
            if (ContentResolver.SCHEME_FILE.equals(uri.getScheme())) {
                new File(uri.getPath()).delete();
            } else {
                activity.getContentResolver().delete(uri, null, null);
            }
        } catch (Exception ignored) {
            // Preserve the original import error.
        }
    }

    private static File uniqueRawFile(File directory, String displayName) {
        File candidate = new File(directory, displayName);
        if (!candidate.exists()) {
            return candidate;
        }
        int dot = displayName.lastIndexOf('.');
        String stem = dot > 0 ? displayName.substring(0, dot) : displayName;
        String suffix = dot > 0 ? displayName.substring(dot) : "";
        for (int index = 1; ; index++) {
            candidate = new File(directory, stem + "-" + index + suffix);
            if (!candidate.exists()) {
                return candidate;
            }
        }
    }

    private static String safeRawName(String requestedName) {
        String name = requestedName == null ? "imported.raw" : requestedName.trim();
        name = name.replace('/', '_').replace('\\', '_').replace('\0', '_');
        return name.isEmpty() ? "imported.raw" : name;
    }

    private static boolean isRawName(String displayName) {
        if (displayName == null) {
            return false;
        }
        int dot = displayName.lastIndexOf('.');
        return dot >= 0 && dot < displayName.length() - 1
                && RAW_SUFFIXES.contains(displayName.substring(dot + 1).toLowerCase(Locale.ROOT));
    }

    private static final class RawLibraryRecord {
        final String uri;
        final String displayName;
        final String displayPath;
        final long bytes;
        final long modifiedSeconds;

        RawLibraryRecord(
                String uri,
                String displayName,
                String displayPath,
                long bytes,
                long modifiedSeconds) {
            this.uri = uri;
            this.displayName = displayName;
            this.displayPath = displayPath;
            this.bytes = bytes;
            this.modifiedSeconds = modifiedSeconds;
        }
    }

    private static final class StoredRaw {
        final Uri uri;
        final String displayName;

        StoredRaw(Uri uri, String displayName) {
            this.uri = uri;
            this.displayName = displayName;
        }
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
        try (Cursor cursor = activity.getContentResolver().query(
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
        try (Cursor cursor = activity.getContentResolver().query(
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
