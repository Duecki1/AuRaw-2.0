package de.duecki.calibraw;

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
import java.util.ArrayList;
import java.util.Arrays;
import java.util.HashSet;
import java.util.Locale;
import java.util.Set;

final class StorageManager {
    private static final String LOG_TAG = "CalibRaw";
    private static final long MAX_RAW_IMPORT_BYTES = 2_000_000_000L;
    private static final long MAX_SIDECAR_BYTES = 32L * 1024L * 1024L;
    private static final int MAX_RAW_LIBRARY_FILES = 20_000;
    private static final int MAX_RAW_LIBRARY_FOLDERS = 10_000;
    private static final int MAX_RAW_LIBRARY_FOLDER_DEPTH = 64;
    private static final long STALE_TEMP_FILE_AGE_MS = 24L * 60L * 60L * 1000L;
    private static final String LEGACY_MEDIASTORE_RAW_RELATIVE_PATH =
            Environment.DIRECTORY_DOWNLOADS + "/CalibRaw/";
    private static final String RAW_PICKER_URI_KEY = "raw-document-uri";

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

    private final AndroidStorageAccess storage;
    private final Callbacks callbacks;
    private final ThumbnailCache thumbnailCache;
    private final PickerLocationStore pickerLocations;
    private volatile String selectedRawLibraryFolder = "";

    StorageManager(AndroidStorageAccess storage, Callbacks callbacks) {
        this.storage = storage;
        this.callbacks = callbacks;
        this.thumbnailCache = new ThumbnailCache(storage);
        this.pickerLocations = new PickerLocationStore(storage);
    }

    Intent createRawDocumentPickerIntent() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        intent.setType("*/*");
        intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, true);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION);
        Uri initialUri = pickerLocations.readContentUri(RAW_PICKER_URI_KEY);
        if (initialUri != null) {
            intent.putExtra(DocumentsContract.EXTRA_INITIAL_URI, initialUri);
        }
        return intent;
    }

    void handleRawDocumentResult(int resultCode, Intent data) {
        if (resultCode != CalibRawActivity.RESULT_OK || data == null) {
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
                uris.size() == 1 ? "CalibRaw document import" : "CalibRaw document batch import")
                .start();
    }

    void scavengeTemporaryRawFiles() {
        long now = System.currentTimeMillis();
        File[] cachedFiles = storage.getCacheDir().listFiles((directory, name) ->
                name.startsWith("calibraw-library-")
                        || name.startsWith("calibraw-import-")
                        || name.startsWith("calibraw-sidecar-")
                        || name.startsWith("calibraw-thumbnail-"));
        deleteStaleFiles(cachedFiles, now);

        File library = rawLibraryDirectory();
        try {
            deleteStaleLibraryTemporaryFiles(library, library.getCanonicalFile(), now, 0);
        } catch (Exception error) {
            Log.w(LOG_TAG, "Could not scavenge stale RAW library temporary files", error);
        }
    }

    private static void deleteStaleFiles(File[] files, long now) {
        if (files == null) {
            return;
        }
        for (File file : files) {
            deleteStaleFile(file, now);
        }
    }

    private static void deleteStaleLibraryTemporaryFiles(
            File directory,
            File canonicalLibrary,
            long now,
            int depth) throws Exception {
        if (depth > MAX_RAW_LIBRARY_FOLDER_DEPTH || !directory.isDirectory()) {
            return;
        }
        File canonicalDirectory = directory.getCanonicalFile();
        if (!canonicalDirectory.toPath().startsWith(canonicalLibrary.toPath())) {
            return;
        }
        File[] entries = directory.listFiles();
        if (entries == null) {
            return;
        }
        for (File entry : entries) {
            if (entry.isDirectory()) {
                deleteStaleLibraryTemporaryFiles(entry, canonicalLibrary, now, depth + 1);
            } else if (AndroidStorageContract.isLibraryTemporaryFileName(entry.getName())) {
                deleteStaleFile(entry, now);
            }
        }
    }

    private static void deleteStaleFile(File file, long now) {
        long modified = file.lastModified();
        boolean isStale = modified > 0L
                && now >= modified
                && now - modified >= STALE_TEMP_FILE_AGE_MS;
        if (file.isFile() && isStale && !file.delete() && file.exists()) {
            file.deleteOnExit();
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
                    pickerLocations.writeContentUri(RAW_PICKER_URI_KEY, uri);
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
            pickerLocations.writeContentUri(RAW_PICKER_URI_KEY, uri);
            deliverLibraryRawFd(stored.uri, stored.displayName);
        } catch (Exception error) {
            if (stored != null) {
                deleteStoredRaw(stored.uri);
            }
            callbacks.onFilePicked("", displayName, "", error.toString(), false);
        }
    }

    private StoredRaw importDocumentIntoLibrary(Uri uri, String displayName) throws Exception {
        if (!AndroidStorageContract.isRawName(displayName)) {
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

    String rawLibraryLocation() {
        return rawLibraryDirectory().getAbsolutePath();
    }

    String listRawLibrary() {
        try {
            return listCombinedRawLibrary();
        } catch (Exception error) {
            throw new IllegalStateException("Could not list the RAW library", error);
        }
    }

    String listRawLibraryFolders() {
        try {
            File root = rawLibraryDirectory();
            StringBuilder result = new StringBuilder();
            appendRawLibraryFolders(root, root, result, 0, new int[]{0});
            return result.toString();
        } catch (Exception error) {
            throw new IllegalStateException("Could not list RAW library folders", error);
        }
    }

    void selectRawLibraryFolder(String relativePath) throws Exception {
        File folder = AndroidStorageContract.libraryFolder(rawLibraryDirectory(), relativePath);
        if (!folder.isDirectory()) {
            throw new IllegalArgumentException("The selected RAW library folder does not exist");
        }
        selectedRawLibraryFolder = AndroidStorageContract.relativeLibraryFolder(
                rawLibraryDirectory(), folder);
    }

    String createRawLibraryFolder(String parentPath, String requestedName) throws Exception {
        File root = rawLibraryDirectory();
        File parent = AndroidStorageContract.libraryFolder(root, parentPath);
        if (!parent.isDirectory()) {
            throw new IllegalArgumentException("The parent RAW library folder does not exist");
        }
        String name = AndroidStorageContract.safeFolderName(requestedName);
        File folder = new File(parent, name);
        if (folder.exists()) {
            throw new IllegalStateException(name + " already exists");
        }
        if (!folder.mkdir()) {
            throw new IllegalStateException("Android storage could not create folder " + name);
        }
        return AndroidStorageContract.relativeLibraryFolder(root, folder);
    }

    int openRawLibraryFd(String uriText) throws Exception {
        Uri uri = Uri.parse(uriText);
        ParcelFileDescriptor descriptor;
        if (ContentResolver.SCHEME_FILE.equals(uri.getScheme())) {
            descriptor = ParcelFileDescriptor.open(
                    new File(uri.getPath()), ParcelFileDescriptor.MODE_READ_ONLY);
        } else {
            descriptor = storage.getContentResolver().openFileDescriptor(uri, "r");
        }
        return NativeFileDescriptors.detach(
                descriptor, "The RAW library returned no file descriptor");
    }

    String rawThumbnailCachePath(
            String uriText,
            long bytes,
            long modifiedSeconds,
            int maximumEdge) throws Exception {
        return thumbnailCache.rawPath(uriText, bytes, modifiedSeconds, maximumEdge);
    }

    String developedThumbnailCachePath(String uriText) throws Exception {
        return thumbnailCache.developedPath(uriText);
    }

    private void copyDevelopedThumbnailCache(String sourceUri, String destinationUri)
            throws Exception {
        thumbnailCache.copyDeveloped(sourceUri, destinationUri);
    }

    void copyRawLibraryDevelopedThumbnail(String sourceUri, String destinationUri)
            throws Exception {
        copyDevelopedThumbnailCache(sourceUri, destinationUri);
    }

    private void clearDevelopedThumbnailCache(String uriText) {
        thumbnailCache.clearDeveloped(uriText);
    }

    void clearThumbnailCache() {
        thumbnailCache.clear();
    }

    long thumbnailCacheSizeBytes() {
        return thumbnailCache.sizeBytes();
    }

    String materializeRawLibraryDocument(String uriText, String displayName) throws Exception {
        Uri uri = Uri.parse(uriText);
        verifyRawLibraryIdentity(uri, displayName);
        String safeName = AndroidStorageContract.safeRawName(displayName);
        int dot = safeName.lastIndexOf('.');
        String suffix = dot >= 0 ? safeName.substring(dot) : ".raw";
        File cached = File.createTempFile("calibraw-library-", suffix, storage.getCacheDir());
        boolean completed = false;
        try {
            try (InputStream input = openLibraryInput(uri);
                 FileOutputStream output = new FileOutputStream(cached)) {
                if (input == null) {
                    throw new IllegalStateException("Android storage returned no RAW stream");
                }
                BoundedStreams.copy(
                        input,
                        output,
                        MAX_RAW_IMPORT_BYTES,
                        storageLimitMessage(MAX_RAW_IMPORT_BYTES));
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

    String materializeRawLibraryThumbnail(String uriText, String displayName) throws Exception {
        return materializeRawLibraryDocument(uriText, displayName);
    }

    void openRawLibraryDocument(String uriText, String displayName) {
        new Thread(
                () -> {
                    try {
                        deliverLibraryRawFd(Uri.parse(uriText), displayName);
                    } catch (Exception error) {
                        callbacks.onFilePickedFd(-1, displayName, uriText, error.toString());
                    }
                },
                "CalibRaw library open").start();
    }

    void removeRawSidecar(String rawUriText, String displayName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        if (!ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())) {
            removeRawSidecarLegacyMediaStore(displayName);
            return;
        }
        AndroidStorageContract.deleteSidecar(
                new File(rawUri.getPath()).getParentFile(), displayName);
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

    String importLocalRawLibraryDocument(String rawPath, String displayName) throws Exception {
        File sourceRaw = new File(rawPath);
        if (!sourceRaw.isFile() || !AndroidStorageContract.isRawName(displayName)) {
            throw new IllegalArgumentException("The local RAW is missing or unsupported");
        }
        StoredRaw imported = null;
        try {
            imported = storeRawInLibrary(Uri.fromFile(sourceRaw), displayName);
            File sourceSidecar = new File(rawPath + ".calibraw");
            if (sourceSidecar.isFile()) {
                publishRawSidecar(
                        sourceSidecar.getAbsolutePath(),
                        imported.uri.toString(),
                        imported.displayName);
            }
            return imported.uri.toString() + "\n" + imported.displayName;
        } catch (Exception error) {
            if (imported != null) {
                try {
                    deleteRawLibraryDocument(imported.uri.toString(), imported.displayName);
                } catch (Exception ignored) {
                }
            }
            throw error;
        }
    }

    void deleteImportedRawLibraryDocument(String rawUri, String displayName) throws Exception {
        deleteRawLibraryDocument(rawUri, displayName);
    }

    String renameRawLibraryDocument(
            String rawUriText,
            String displayName,
            String requestedName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        String safeName = AndroidStorageContract.safeRawName(requestedName);
        if (!safeName.equals(requestedName) || !AndroidStorageContract.isRawName(requestedName)) {
            throw new IllegalArgumentException("Enter a safe supported RAW filename");
        }
        if (!ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())) {
            throw new IllegalStateException(
                    "This legacy Android library item must finish migrating before it can be renamed");
        }

        File sourceRaw = new File(rawUri.getPath());
        if (displayName.equals(requestedName)) {
            return rawUri.toString();
        }
        File parent = sourceRaw.getParentFile();
        File destinationRaw = new File(parent, requestedName);
        File sourceSidecar = new File(parent, AndroidStorageContract.sidecarDisplayName(displayName));
        File destinationSidecar = new File(parent, AndroidStorageContract.sidecarDisplayName(requestedName));
        if (destinationRaw.exists() || destinationSidecar.exists()) {
            throw new IllegalStateException(requestedName + " already exists");
        }
        if (!sourceRaw.renameTo(destinationRaw)) {
            throw new IllegalStateException("Android storage could not rename the RAW file");
        }
        if (sourceSidecar.isFile() && !sourceSidecar.renameTo(destinationSidecar)) {
            if (!destinationRaw.renameTo(sourceRaw)) {
                throw new IllegalStateException(
                        "The RAW was renamed, but its sidecar and RAW rollback both failed");
            }
            throw new IllegalStateException("Android storage could not rename the RAW sidecar");
        }
        String destinationUri = Uri.fromFile(destinationRaw).toString();
        try {
            copyDevelopedThumbnailCache(rawUriText, destinationUri);
        } catch (Exception error) {
            Log.w(LOG_TAG, "Renamed RAW but could not preserve its developed thumbnail cache", error);
            clearDevelopedThumbnailCache(destinationUri);
        }
        clearDevelopedThumbnailCache(rawUriText);
        return destinationUri;
    }

    void deleteRawLibraryDocument(String rawUriText, String displayName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        boolean fileBacked = ContentResolver.SCHEME_FILE.equals(rawUri.getScheme());
        File raw = fileBacked ? new File(rawUri.getPath()) : null;
        if (fileBacked) {
            if (raw.exists() && !raw.delete()) {
                throw new IllegalStateException("Could not delete the RAW file");
            }
        } else if (storage.getContentResolver().delete(rawUri, null, null) <= 0) {
            throw new IllegalStateException("Android storage could not delete the RAW file");
        }

        try {
            if (fileBacked) {
                AndroidStorageContract.deleteSidecar(raw.getParentFile(), displayName);
            } else {
                removeRawSidecarLegacyMediaStore(displayName);
            }
        } catch (Exception error) {
            Log.w(LOG_TAG, "Deleted RAW but could not clean up its sidecar", error);
        }
        clearDevelopedThumbnailCache(rawUriText);
    }

    String materializeRawSidecar(String rawUriText, String displayName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        if (!ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())) {
            return materializeRawSidecarLegacyMediaStore(displayName);
        }
        File sidecar = new File(
                new File(rawUri.getPath()).getParentFile(), AndroidStorageContract.sidecarDisplayName(displayName));
        if (!sidecar.isFile()) {
            return "";
        }

        File cached = File.createTempFile("calibraw-sidecar-", ".calibraw", storage.getCacheDir());
        boolean completed = false;
        try {
            try (FileInputStream input = new FileInputStream(sidecar);
                 FileOutputStream output = new FileOutputStream(cached)) {
                BoundedStreams.copy(
                        input,
                        output,
                        MAX_SIDECAR_BYTES,
                        storageLimitMessage(MAX_SIDECAR_BYTES));
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
        File cached = File.createTempFile("calibraw-sidecar-", ".calibraw", storage.getCacheDir());
        boolean completed = false;
        try {
            try (InputStream input = storage.getContentResolver().openInputStream(newestGeneration);
                 FileOutputStream output = new FileOutputStream(cached)) {
                if (input == null) {
                    throw new IllegalStateException("Android storage returned no sidecar stream");
                }
                BoundedStreams.copy(
                        input,
                        output,
                        MAX_SIDECAR_BYTES,
                        storageLimitMessage(MAX_SIDECAR_BYTES));
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

    String createRawSidecarCache() throws Exception {
        return File.createTempFile("calibraw-sidecar-", ".calibraw", storage.getCacheDir()).getAbsolutePath();
    }

    String publishRawSidecar(
            String cachedPath,
            String rawUriText,
            String displayName) throws Exception {
        File cached = new File(cachedPath);
        if (!cached.isFile() || cached.length() > MAX_SIDECAR_BYTES) {
            throw new IllegalStateException("CalibRaw sidecar staging file is missing or too large");
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
        ContentResolver resolver = storage.getContentResolver();
        String displayName = AndroidStorageContract.sidecarDisplayName(rawDisplayName);
        String stagedName = AndroidStorageContract.sidecarStagePrefix(rawDisplayName)
                + Long.toUnsignedString(System.nanoTime());
        ArrayList<Uri> oldSidecars = legacyMediaStoreSidecarUris(rawDisplayName);
        ContentValues values = new ContentValues();
        values.put(MediaStore.Downloads.DISPLAY_NAME, stagedName);
        // A known MIME may make MediaProvider rewrite CalibRaw's custom extension.
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
                BoundedStreams.copy(
                        input,
                        output,
                        MAX_SIDECAR_BYTES,
                        storageLimitMessage(MAX_SIDECAR_BYTES));
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
            // A published staging row is a valid recovery generation; preserve it.
            if (!contentPublished) {
                resolver.delete(destination, null, null);
            }
        }
    }

    private String publishRawSidecarFile(
            File cached,
            File directory,
            String rawDisplayName) throws Exception {
        return AndroidStorageContract.publishSidecarAtomically(
                cached, directory, rawDisplayName, MAX_SIDECAR_BYTES);
    }

    private ArrayList<Uri> legacyMediaStoreSidecarUris(String rawDisplayName) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Legacy MediaStore sidecars require Android 10 or newer");
        }
        ArrayList<Uri> result = new ArrayList<>();
        String displayName = AndroidStorageContract.sidecarDisplayName(rawDisplayName);
        String stagedPrefix = AndroidStorageContract.sidecarStagePrefix(rawDisplayName);
        String[] projection = {
                MediaStore.Downloads._ID,
                MediaStore.Downloads.DISPLAY_NAME
        };
        String selection = legacyMediaStoreSidecarSelection();
        String[] args = legacyMediaStoreSidecarSelectionArgs(displayName, stagedPrefix);
        try (Cursor cursor = storage.getContentResolver().query(
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
        String displayName = AndroidStorageContract.sidecarDisplayName(rawDisplayName);
        String stagedPrefix = AndroidStorageContract.sidecarStagePrefix(rawDisplayName);
        String selection = MediaStore.Downloads._ID + "=? AND ("
                + legacyMediaStoreSidecarSelection() + ")";
        String[] sidecarArgs = legacyMediaStoreSidecarSelectionArgs(displayName, stagedPrefix);
        String[] args = new String[sidecarArgs.length + 1];
        args[0] = Long.toString(generationId);
        System.arraycopy(sidecarArgs, 0, args, 1, sidecarArgs.length);
        return storage.getContentResolver().delete(
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
                storage.getPackageName(),
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
        File raw = new File(rawUri.getPath());
        if (!AndroidStorageContract.isAllowedRawFile(
                raw, expectedDisplayName, rawLibraryDirectory(), externalMediaRootDirectory())) {
            throw new IllegalArgumentException("The RAW is outside CalibRaw's library");
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
                || !AndroidStorageContract.isRawName(expectedDisplayName)
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
        try (Cursor cursor = storage.getContentResolver().query(
                rawUri,
                projection,
                null,
                null,
                null)) {
            if (cursor == null || !cursor.moveToFirst()) {
                throw new IllegalArgumentException("The RAW is not in CalibRaw's library");
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
            if (!AndroidStorageContract.isAllowedLegacyMediaStoreRow(
                    expectedId,
                    storedId,
                    expectedDisplayName,
                    storedName,
                    LEGACY_MEDIASTORE_RAW_RELATIVE_PATH,
                    storedPath,
                    storage.getPackageName(),
                    storedOwner,
                    pending,
                    cursor.moveToNext())) {
                throw new IllegalArgumentException("The RAW is outside CalibRaw's library");
            }
        }
    }

    private static String escapeLike(String value) {
        return value.replace("\\", "\\\\").replace("%", "\\%").replace("_", "\\_");
    }

    private StoredRaw storeRawInLibrary(Uri source, String requestedName) throws Exception {
        return storeRawFile(source, requestedName);
    }

    private StoredRaw storeRawFile(Uri source, String requestedName) throws Exception {
        File directory = selectedRawLibraryDirectory();
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create " + directory);
        }
        File destination = uniqueRawFile(directory, AndroidStorageContract.safeRawName(requestedName));
        File partial = uniqueRawFile(
                directory,
                AndroidStorageContract.importPartialName(destination.getName()));
        boolean completed = false;
        try {
            try (InputStream input = openLibraryInput(source);
                 FileOutputStream output = new FileOutputStream(partial)) {
                if (input == null) {
                    throw new IllegalStateException("The document provider returned no input stream");
                }
                BoundedStreams.copy(
                        input,
                        output,
                        MAX_RAW_IMPORT_BYTES,
                        storageLimitMessage(MAX_RAW_IMPORT_BYTES));
                output.getFD().sync();
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
                NativeFileDescriptors.closeTransferred(fd);
            }
        }
    }

    private InputStream openLibraryInput(Uri uri) throws Exception {
        if (ContentResolver.SCHEME_FILE.equals(uri.getScheme())) {
            return new FileInputStream(new File(uri.getPath()));
        }
        return storage.getContentResolver().openInputStream(uri);
    }

    private String listCombinedRawLibrary() {
        ArrayList<RawLibraryRecord> records = new ArrayList<>();
        records.addAll(listFileRawLibrary(selectedRawLibraryDirectory()));
        if (selectedRawLibraryFolder.isEmpty()) {
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

    private void appendRawLibraryFolders(
            File root,
            File directory,
            StringBuilder result,
            int depth,
            int[] count) throws Exception {
        if (depth >= MAX_RAW_LIBRARY_FOLDER_DEPTH || count[0] >= MAX_RAW_LIBRARY_FOLDERS) {
            return;
        }
        File[] children = directory.listFiles(file ->
                file.isDirectory() && !file.getName().startsWith("."));
        if (children == null) {
            return;
        }
        Arrays.sort(children, (left, right) ->
                left.getName().compareToIgnoreCase(right.getName()));
        for (File child : children) {
            if (count[0] >= MAX_RAW_LIBRARY_FOLDERS) {
                return;
            }
            String relative = AndroidStorageContract.relativeLibraryFolder(root, child);
            result.append(Uri.encode(relative)).append('\t')
                    .append(Uri.encode(child.getName())).append('\n');
            count[0]++;
            appendRawLibraryFolders(root, child, result, depth + 1, count);
        }
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
        String[] selectionArgs = {LEGACY_MEDIASTORE_RAW_RELATIVE_PATH, storage.getPackageName()};
        try (Cursor cursor = storage.getContentResolver().query(
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
            // One sentinel beyond the UI limit distinguishes exact-size from truncation.
            while (result.size() <= MAX_RAW_LIBRARY_FILES && cursor.moveToNext()) {
                String name = cursor.getString(nameColumn);
                if (!AndroidStorageContract.isRawName(name)) {
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
            // Preserve one sentinel beyond the UI limit.
            if (result.size() > MAX_RAW_LIBRARY_FILES) {
                break;
            }
            if (!file.isFile() || !AndroidStorageContract.isRawName(file.getName())) {
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
        try (Cursor cursor = storage.getContentResolver().query(uri, projection, null, null, null)) {
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
        File[] mediaDirectories = storage.getExternalMediaDirs();
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
        File directory = AndroidStorageContract.rawLibraryDirectory(externalMediaRootDirectory());
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create " + directory);
        }
        File noMedia = AndroidStorageContract.noMediaMarker(directory);
        try {
            if (!noMedia.exists() && !noMedia.createNewFile()) {
                Log.w(LOG_TAG, "Could not create .nomedia marker in " + directory);
            }
        } catch (Exception error) {
            Log.w(LOG_TAG, "Could not create .nomedia marker in " + directory, error);
        }
        return directory;
    }

    private File selectedRawLibraryDirectory() {
        try {
            File directory = AndroidStorageContract.libraryFolder(
                    rawLibraryDirectory(), selectedRawLibraryFolder);
            if (!directory.isDirectory()) {
                throw new IllegalStateException("The selected RAW library folder no longer exists");
            }
            return directory;
        } catch (RuntimeException error) {
            throw error;
        } catch (Exception error) {
            throw new IllegalStateException("The selected RAW library folder is invalid", error);
        }
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
        }, "CalibRaw RAW library migration").start();
    }

    private void migrateLegacyExternalMediaRoot() {
        File root = externalMediaRootDirectory();
        File library = rawLibraryDirectory();
        File[] files = root.listFiles();
        if (files == null) {
            return;
        }
        for (File source : files) {
            if (!source.isFile() || !AndroidStorageContract.isRawName(source.getName())) {
                continue;
            }
            File destination = new File(library, source.getName());
            if (destination.exists()) {
                continue;
            }
            try {
                AndroidStorageContract.moveOrCopyLegacyFile(source, destination, MAX_RAW_IMPORT_BYTES);
            } catch (Exception error) {
                Log.w(LOG_TAG, "Could not migrate legacy file RAW " + source.getName(), error);
                continue;
            }

            File sourceSidecar = new File(root, AndroidStorageContract.sidecarDisplayName(source.getName()));
            File destinationSidecar = new File(library, AndroidStorageContract.sidecarDisplayName(source.getName()));
            if (sourceSidecar.isFile() && !destinationSidecar.exists()) {
                try {
                    AndroidStorageContract.moveOrCopyLegacyFile(sourceSidecar, destinationSidecar, MAX_SIDECAR_BYTES);
                } catch (Exception error) {
                    Log.w(LOG_TAG, "Could not migrate legacy file sidecar " + sourceSidecar.getName(), error);
                }
            }
        }

        // Recover a sidecar stranded after its RAW moved in a previous failed migration.
        for (File sourceSidecar : files) {
            String sidecarName = sourceSidecar.getName();
            if (!sourceSidecar.isFile() || !sidecarName.endsWith(".calibraw")) {
                continue;
            }
            String rawName = sidecarName.substring(0, sidecarName.length() - ".calibraw".length());
            if (new File(root, rawName).exists()) {
                continue;
            }
            File destinationRaw = new File(library, rawName);
            File destinationSidecar = new File(library, sidecarName);
            if (!destinationRaw.isFile() || destinationSidecar.exists()) {
                continue;
            }
            try {
                AndroidStorageContract.moveOrCopyLegacyFile(sourceSidecar, destinationSidecar, MAX_SIDECAR_BYTES);
            } catch (Exception error) {
                Log.w(LOG_TAG, "Could not recover legacy file sidecar " + sidecarName, error);
            }
        }
    }

    private void migrateLegacyMediaStoreRawLibrary() {
        for (RawLibraryRecord record : listLegacyMediaStoreRawLibrary()) {
            Uri source = Uri.parse(record.uri);
            File destination = new File(rawLibraryDirectory(), AndroidStorageContract.safeRawName(record.displayName));
            if (destination.exists()) {
                continue;
            }
            File partial = new File(
                    rawLibraryDirectory(), ".calibraw-migrate-" + destination.getName() + ".part");
            String cachedSidecar = "";
            boolean rawPublished = false;
            try {
                try (InputStream input = storage.getContentResolver().openInputStream(source);
                     FileOutputStream output = new FileOutputStream(partial)) {
                    if (input == null) {
                        throw new IllegalStateException("Android storage returned no legacy RAW stream");
                    }
                    BoundedStreams.copy(
                            input,
                            output,
                            MAX_RAW_IMPORT_BYTES,
                            storageLimitMessage(MAX_RAW_IMPORT_BYTES));
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

                if (storage.getContentResolver().delete(source, null, null) <= 0) {
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
                            rawLibraryDirectory(), AndroidStorageContract.sidecarDisplayName(destination.getName()));
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

    private void deleteStoredRaw(Uri uri) {
        try {
            if (ContentResolver.SCHEME_FILE.equals(uri.getScheme())) {
                new File(uri.getPath()).delete();
            } else {
                storage.getContentResolver().delete(uri, null, null);
            }
        } catch (Exception ignored) {
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

    private static String storageLimitMessage(long maximumBytes) {
        return "The document exceeds the " + maximumBytes + "-byte import limit";
    }

    private Long queryDocumentSize(Uri uri) {
        try (Cursor cursor = storage.getContentResolver().query(
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
            // Streaming bounds remain authoritative when provider metadata is unreliable.
        }
        return null;
    }

    private String queryDisplayName(Uri uri) {
        try (Cursor cursor = storage.getContentResolver().query(
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
            // The URI may remain readable when its optional display-name query fails.
        }
        return "selected RAW";
    }

}
