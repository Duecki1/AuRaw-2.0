package de.duecki.auraw;

import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.provider.DocumentsContract;
import android.util.Log;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.file.Files;
import java.util.Locale;

final class ProfileImporter {
    private static final String LOG_TAG = "AuRaw";
    private static final long MAX_DCP_FILE_BYTES = 64L * 1024L * 1024L;
    private static final long MAX_DCP_TREE_BYTES = 1024L * 1024L * 1024L;
    private static final int MAX_DCP_FILES = 10_000;
    private static final int MAX_DCP_TREE_DEPTH = 16;
    private static final String CAMERA_PROFILE_MIRROR_PREFIX = "camera-profiles-";
    private static final String CAMERA_PROFILE_PICKER_URI_KEY = "camera-profile-tree-uri";

    interface Callbacks {
        void onImportStarted(String displayName);
        void onFolderPicked(String cachedPath, String displayName, int profileCount, String error);
    }

    private final AndroidStorageAccess storage;
    private final Callbacks callbacks;
    private final PickerLocationStore pickerLocations;

    ProfileImporter(AndroidStorageAccess storage, Callbacks callbacks) {
        this.storage = storage;
        this.callbacks = callbacks;
        this.pickerLocations = new PickerLocationStore(storage);
    }

    Intent createFolderPickerIntent() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
        intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION
                | Intent.FLAG_GRANT_PREFIX_URI_PERMISSION);
        Uri initialUri = pickerLocations.readContentUri(CAMERA_PROFILE_PICKER_URI_KEY);
        if (initialUri != null) {
            intent.putExtra(DocumentsContract.EXTRA_INITIAL_URI, initialUri);
        }
        return intent;
    }

    void clearFolderPickerLocation() {
        pickerLocations.clear(CAMERA_PROFILE_PICKER_URI_KEY);
    }

    void handleFolderPickerResult(int resultCode, Intent data) {
        if (resultCode != AuRawActivity.RESULT_OK || data == null || data.getData() == null) {
            callbacks.onFolderPicked("", "", 0, "");
            return;
        }
        Uri treeUri = data.getData();
        String folderLabel = queryProfileFolderName(treeUri);
        callbacks.onImportStarted(folderLabel);
        new Thread(
                () -> importCameraProfileFolder(treeUri, folderLabel),
                "AuRaw camera profile import")
                .start();
    }

    void removeCameraProfileMirror(String mirrorPath) {
        final String requestedPath = mirrorPath == null ? "" : mirrorPath;
        new Thread(
                () -> {
                    try {
                        removeOwnedCameraProfileMirror(requestedPath);
                    } catch (Exception error) {
                        Log.w(LOG_TAG, "Could not remove superseded camera-profile mirror", error);
                    }
                },
                "AuRaw camera profile cleanup")
                .start();
    }

    private void importCameraProfileFolder(Uri treeUri, String label) {
        File destination = new File(
                storage.getFilesDir(),
                CAMERA_PROFILE_MIRROR_PREFIX + Long.toUnsignedString(System.nanoTime()));
        try {
            if (!destination.mkdirs()) {
                throw new IllegalStateException("Could not create private camera-profile storage");
            }
            ProfileImportStats stats = new ProfileImportStats();
            String rootDocumentId = DocumentsContract.getTreeDocumentId(treeUri);
            copyCameraProfileTree(treeUri, rootDocumentId, destination, 0, stats);
            if (stats.files == 0) {
                deleteDirectoryTree(destination);
                throw new IllegalArgumentException(
                        "The selected folder contains no .dcp camera profiles");
            }
            pickerLocations.writeContentUri(CAMERA_PROFILE_PICKER_URI_KEY, treeUri);
            String importedPath = destination.getAbsolutePath();
            int importedProfiles = stats.files;
            storage.runOnUiThread(() -> callbacks.onFolderPicked(
                    importedPath, label, importedProfiles, ""));
        } catch (Exception error) {
            deleteDirectoryTree(destination);
            String message = error.toString();
            storage.runOnUiThread(() -> callbacks.onFolderPicked("", label, 0, message));
        }
    }

    private void copyCameraProfileTree(
            Uri treeUri,
            String parentDocumentId,
            File destination,
            int depth,
            ProfileImportStats stats) throws Exception {
        if (depth > MAX_DCP_TREE_DEPTH) {
            throw new IllegalStateException(
                    "Camera profile folder nesting exceeds " + MAX_DCP_TREE_DEPTH + " levels");
        }
        Uri childrenUri = DocumentsContract.buildChildDocumentsUriUsingTree(
                treeUri, parentDocumentId);
        String[] projection = {
                DocumentsContract.Document.COLUMN_DOCUMENT_ID,
                DocumentsContract.Document.COLUMN_DISPLAY_NAME,
                DocumentsContract.Document.COLUMN_MIME_TYPE,
                DocumentsContract.Document.COLUMN_SIZE
        };
        try (Cursor cursor = storage.getContentResolver().query(
                childrenUri, projection, null, null, null)) {
            if (cursor == null) {
                throw new IllegalStateException("Android storage could not list the selected folder");
            }
            int idColumn = cursor.getColumnIndexOrThrow(
                    DocumentsContract.Document.COLUMN_DOCUMENT_ID);
            int nameColumn = cursor.getColumnIndexOrThrow(
                    DocumentsContract.Document.COLUMN_DISPLAY_NAME);
            int typeColumn = cursor.getColumnIndexOrThrow(
                    DocumentsContract.Document.COLUMN_MIME_TYPE);
            int sizeColumn = cursor.getColumnIndex(
                    DocumentsContract.Document.COLUMN_SIZE);
            while (cursor.moveToNext()) {
                String documentId = cursor.getString(idColumn);
                String name = cursor.getString(nameColumn);
                String mimeType = cursor.getString(typeColumn);
                if (documentId == null || name == null) {
                    continue;
                }
                String safeName = safeProfileComponent(name);
                if (DocumentsContract.Document.MIME_TYPE_DIR.equals(mimeType)) {
                    File childDirectory = uniqueProfileDirectory(destination, safeName, documentId);
                    if (!childDirectory.mkdirs() && !childDirectory.isDirectory()) {
                        throw new IllegalStateException(
                                "Could not create camera-profile subfolder " + safeName);
                    }
                    copyCameraProfileTree(
                            treeUri, documentId, childDirectory, depth + 1, stats);
                    File[] contents = childDirectory.listFiles();
                    if (contents != null && contents.length == 0) {
                        childDirectory.delete();
                    }
                    continue;
                }
                if (!name.toLowerCase(Locale.ROOT).endsWith(".dcp")) {
                    continue;
                }
                if (stats.files >= MAX_DCP_FILES) {
                    throw new IllegalStateException(
                            "The selected folder contains more than " + MAX_DCP_FILES
                                    + " DCP files");
                }
                if (sizeColumn >= 0 && !cursor.isNull(sizeColumn)) {
                    long declaredSize = cursor.getLong(sizeColumn);
                    if (declaredSize > MAX_DCP_FILE_BYTES) {
                        throw new IllegalStateException(
                                name + " exceeds the per-profile import limit");
                    }
                    if (declaredSize >= 0 && stats.bytes > MAX_DCP_TREE_BYTES - declaredSize) {
                        throw new IllegalStateException(
                                "The selected profile tree exceeds the "
                                        + MAX_DCP_TREE_BYTES + "-byte import limit");
                    }
                }
                Uri documentUri = DocumentsContract.buildDocumentUriUsingTree(treeUri, documentId);
                File output = uniqueProfileFile(destination, safeName, documentId);
                long copied = 0L;
                try (InputStream input = storage.getContentResolver().openInputStream(documentUri);
                     FileOutputStream stream = new FileOutputStream(output)) {
                    if (input == null) {
                        throw new IllegalStateException("Android storage returned no DCP stream");
                    }
                    copied = copyProfile(input, stream, stats.bytes);
                    stream.getFD().sync();
                } catch (Exception error) {
                    output.delete();
                    throw error;
                }
                stats.bytes += copied;
                stats.files++;
            }
        }
    }

    private static long copyProfile(InputStream input, OutputStream output, long alreadyImported)
            throws Exception {
        byte[] buffer = new byte[256 * 1024];
        long fileBytes = 0L;
        while (true) {
            int count = input.read(buffer);
            if (count < 0) {
                break;
            }
            if (count == 0) {
                int value = input.read();
                if (value < 0) {
                    break;
                }
                fileBytes = checkedProfileCopyLength(fileBytes, 1, alreadyImported);
                output.write(value);
                continue;
            }
            fileBytes = checkedProfileCopyLength(fileBytes, count, alreadyImported);
            output.write(buffer, 0, count);
        }
        return fileBytes;
    }

    private static long checkedProfileCopyLength(
            long fileBytes, int count, long alreadyImported) {
        if (count < 0 || fileBytes > MAX_DCP_FILE_BYTES - count) {
            throw new IllegalStateException(
                    "A DCP exceeds the " + MAX_DCP_FILE_BYTES + "-byte import limit");
        }
        long next = fileBytes + count;
        if (alreadyImported > MAX_DCP_TREE_BYTES - next) {
            throw new IllegalStateException(
                    "The selected profile tree exceeds the "
                            + MAX_DCP_TREE_BYTES + "-byte import limit");
        }
        return next;
    }

    private static String safeProfileComponent(String requestedName) {
        String name = requestedName == null ? "profile" : requestedName.trim();
        name = name.replaceAll("[\\\\/:*?\"<>|\\p{Cntrl}]", "_");
        if (name.isEmpty() || ".".equals(name) || "..".equals(name)) {
            name = "profile";
        }
        name = AndroidStorageContract.truncateUtf8PreservingExtension(name, 180);
        return name;
    }

    private static File uniqueProfileDirectory(File parent, String name, String documentId) {
        File candidate = new File(parent, name);
        if (!candidate.exists()) {
            return candidate;
        }
        return new File(parent, name + "-" + Integer.toHexString(documentId.hashCode()));
    }

    private static File uniqueProfileFile(File parent, String name, String documentId) {
        File candidate = new File(parent, name);
        if (!candidate.exists()) {
            return candidate;
        }
        int dot = name.toLowerCase(Locale.ROOT).endsWith(".dcp") ? name.length() - 4 : name.length();
        String stem = name.substring(0, dot);
        return new File(
                parent,
                stem + "-" + Integer.toHexString(documentId.hashCode()) + ".dcp");
    }

    private String queryProfileFolderName(Uri uri) {
        try (Cursor cursor = storage.getContentResolver().query(
                uri,
                new String[]{DocumentsContract.Document.COLUMN_DISPLAY_NAME},
                null,
                null,
                null)) {
            if (cursor != null && cursor.moveToFirst()) {
                int column = cursor.getColumnIndex(
                        DocumentsContract.Document.COLUMN_DISPLAY_NAME);
                if (column >= 0) {
                    String name = cursor.getString(column);
                    if (name != null && !name.isEmpty()) {
                        return name;
                    }
                }
            }
        } catch (Exception ignored) {
        }
        return "CameraProfiles";
    }

    private void removeOwnedCameraProfileMirror(String mirrorPath) throws Exception {
        if (mirrorPath.isEmpty()) {
            return;
        }
        File requestedMirror = new File(mirrorPath);
        if (Files.isSymbolicLink(requestedMirror.toPath())) {
            throw new IllegalArgumentException(
                    "Refusing to follow a camera-profile mirror symbolic link");
        }
        File filesDirectory = storage.getFilesDir().getCanonicalFile();
        File mirror = requestedMirror.getCanonicalFile();
        if (!isCameraProfileMirrorName(mirror.getName())
                || mirror.getParentFile() == null
                || !filesDirectory.equals(mirror.getParentFile())) {
            throw new IllegalArgumentException(
                    "Refusing to remove a path outside AuRaw camera-profile storage");
        }
        deleteDirectoryTreeChecked(mirror);
    }

    private static boolean isCameraProfileMirrorName(String name) {
        if (!name.startsWith(CAMERA_PROFILE_MIRROR_PREFIX)) {
            return false;
        }
        int suffixStart = CAMERA_PROFILE_MIRROR_PREFIX.length();
        if (name.length() == suffixStart) {
            return false;
        }
        for (int index = suffixStart; index < name.length(); index++) {
            char value = name.charAt(index);
            if (value < '0' || value > '9') {
                return false;
            }
        }
        return true;
    }

    private static void deleteDirectoryTreeChecked(File file) throws Exception {
        boolean symbolicLink = Files.isSymbolicLink(file.toPath());
        if (!symbolicLink && !file.exists()) {
            return;
        }
        if (!symbolicLink && file.isDirectory()) {
            File[] children = file.listFiles();
            if (children == null) {
                throw new IllegalStateException("Could not list camera-profile mirror " + file);
            }
            for (File child : children) {
                deleteDirectoryTreeChecked(child);
            }
        }
        if (!Files.deleteIfExists(file.toPath()) && file.exists()) {
            throw new IllegalStateException("Could not remove camera-profile mirror " + file);
        }
    }

    private static void deleteDirectoryTree(File file) {
        if (file == null) {
            return;
        }
        boolean symbolicLink = Files.isSymbolicLink(file.toPath());
        if (!symbolicLink && !file.exists()) {
            return;
        }
        if (!symbolicLink) {
            File[] children = file.listFiles();
            if (children != null) {
                for (File child : children) {
                    deleteDirectoryTree(child);
                }
            }
        }
        file.delete();
    }

    private static final class ProfileImportStats {
        int files;
        long bytes;
    }
}
