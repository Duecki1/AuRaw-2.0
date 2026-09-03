package de.duecki.calibraw;

import android.content.ClipData;
import android.content.ContentResolver;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.ParcelFileDescriptor;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;
import android.util.Log;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
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
        return storeRawFile(uri, displayName);
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

    void copyRawLibraryDevelopedThumbnail(String sourceUri, String destinationUri)
            throws Exception {
        thumbnailCache.copyDeveloped(sourceUri, destinationUri);
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
        AndroidStorageContract.deleteSidecar(
                new File(rawUri.getPath()).getParentFile(), displayName);
    }

    String importLocalRawLibraryDocument(String rawPath, String displayName) throws Exception {
        File sourceRaw = new File(rawPath);
        if (!sourceRaw.isFile() || !AndroidStorageContract.isRawName(displayName)) {
            throw new IllegalArgumentException("The local RAW is missing or unsupported");
        }
        StoredRaw imported = null;
        try {
            imported = storeRawFile(Uri.fromFile(sourceRaw), displayName);
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
            thumbnailCache.copyDeveloped(rawUriText, destinationUri);
        } catch (Exception error) {
            Log.w(LOG_TAG, "Renamed RAW but could not preserve its developed thumbnail cache", error);
            thumbnailCache.clearDeveloped(destinationUri);
        }
        thumbnailCache.clearDeveloped(rawUriText);
        return destinationUri;
    }

    void deleteRawLibraryDocument(String rawUriText, String displayName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        File raw = new File(rawUri.getPath());
        if (raw.exists() && !raw.delete()) {
            throw new IllegalStateException("Could not delete the RAW file");
        }

        try {
            AndroidStorageContract.deleteSidecar(raw.getParentFile(), displayName);
        } catch (Exception error) {
            Log.w(LOG_TAG, "Deleted RAW but could not clean up its sidecar", error);
        }
        thumbnailCache.clearDeveloped(rawUriText);
    }

    String materializeRawSidecar(String rawUriText, String displayName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
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
        return AndroidStorageContract.publishSidecarAtomically(
                cached, new File(rawUri.getPath()).getParentFile(), displayName, MAX_SIDECAR_BYTES);
    }

    private void verifyRawLibraryIdentity(Uri rawUri, String expectedDisplayName) throws Exception {
        verifyFileRawLibraryIdentity(rawUri, expectedDisplayName);
    }

    private void verifyFileRawLibraryIdentity(
            Uri rawUri,
            String expectedDisplayName) throws Exception {
        if (rawUri.getPath() == null || expectedDisplayName == null) {
            throw new IllegalArgumentException("The RAW library URI is invalid");
        }
        File raw = new File(rawUri.getPath());
        if (!AndroidStorageContract.isAllowedRawFile(
                raw, expectedDisplayName, rawLibraryDirectory())) {
            throw new IllegalArgumentException("The RAW is outside CalibRaw's library");
        }
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

    private void deleteStoredRaw(Uri uri) {
        try {
            new File(uri.getPath()).delete();
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
