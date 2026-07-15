package de.duecki.auraw;

import android.Manifest;
import android.app.NativeActivity;
import android.content.ContentUris;
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
import android.os.ParcelFileDescriptor;
import android.provider.MediaStore;
import android.provider.OpenableColumns;

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

public final class AuRawActivity extends NativeActivity {
    private static final int OPEN_RAW_DOCUMENT = 1001;
    private static final int WRITE_EXPORT_PERMISSION = 1002;
    private static final long MAX_RAW_IMPORT_BYTES = 2_000_000_000L;
    private static final long MAX_SIDECAR_BYTES = 32L * 1024L * 1024L;
    private static final int MAX_RAW_LIBRARY_FILES = 20_000;
    private static final long STALE_TEMP_FILE_AGE_MS = 24L * 60L * 60L * 1000L;
    private static final String RAW_LIBRARY_RELATIVE_PATH =
            Environment.DIRECTORY_DOWNLOADS + "/AuRaw/";
    private static final Set<String> RAW_SUFFIXES = new HashSet<>(Arrays.asList(
            "3fr", "ari", "arw", "bay", "bmq", "cap", "cine", "cr2", "cr3", "crw",
            "cs1", "dc2", "dcr", "dcs", "dng", "drf", "eip", "erf", "fff", "gpr",
            "iiq", "k25", "kc2", "kdc", "mdc", "mef", "mos", "mrw", "nef", "nrw",
            "obm", "orf", "pef", "ptx", "pxn", "qtk", "r3d", "raf", "raw", "rdc",
            "rw2", "rwl", "rwz", "sr2", "srf", "srw", "sti", "x3f"));

    private String pendingExportPath;
    private String pendingExportName;

    static {
        System.loadLibrary("auraw");
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        scavengeTemporaryRawFiles();
    }

    private void scavengeTemporaryRawFiles() {
        File[] cachedFiles = getCacheDir().listFiles((directory, name) ->
                name.startsWith("auraw-library-")
                        || name.startsWith("auraw-import-")
                        || name.startsWith("auraw-sidecar-"));
        deleteStaleFiles(cachedFiles);

        File[] partialImports = legacyRawLibraryDirectory().listFiles((directory, name) ->
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

    private static native void nativeOnFilePicked(
            String cachedPath,
            String displayName,
            String libraryUri,
            String error,
            boolean temporary);

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
            nativeOnFilePicked("", "", "", "", false);
            return;
        }

        Uri uri = data.getData();
        String displayName = queryDisplayName(uri);
        new Thread(
                () -> importDocument(uri, displayName),
                "AuRaw document import").start();
    }

    private void importDocument(Uri uri, String displayName) {
        StoredRaw stored = null;
        try {
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
            stored = storeRawInLibrary(uri, displayName);
            materializeLibraryRaw(stored.uri, stored.displayName);
        } catch (Exception error) {
            if (stored != null) {
                deleteStoredRaw(stored.uri);
            }
            nativeOnFilePicked("", displayName, "", error.toString(), false);
        }
    }

    /** Human-readable storage location shown by the Rust library UI. */
    public String rawLibraryLocation() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            return RAW_LIBRARY_RELATIVE_PATH.substring(0, RAW_LIBRARY_RELATIVE_PATH.length() - 1);
        }
        return legacyRawLibraryDirectory().getAbsolutePath();
    }

    /**
     * Lists only AuRaw's scoped-storage collection. Each line contains URI,
     * name, visible path, bytes, and modified time; strings are URI-escaped so
     * tabs and newlines in document names cannot corrupt the bridge format.
     */
    public String listRawLibrary() {
        try {
            return Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q
                    ? listScopedRawLibrary()
                    : listLegacyRawLibrary();
        } catch (Exception error) {
            throw new IllegalStateException("Could not list the RAW library", error);
        }
    }

    /** Returns an owned native descriptor; Rust closes it after thumbnail extraction. */
    public int openRawLibraryFd(String uriText) throws Exception {
        Uri uri = Uri.parse(uriText);
        ParcelFileDescriptor descriptor;
        if (ContentResolver.SCHEME_FILE.equals(uri.getScheme())) {
            descriptor = ParcelFileDescriptor.open(
                    new File(uri.getPath()), ParcelFileDescriptor.MODE_READ_ONLY);
        } else {
            descriptor = getContentResolver().openFileDescriptor(uri, "r");
        }
        if (descriptor == null) {
            throw new IllegalStateException("The RAW library returned no file descriptor");
        }
        return descriptor.detachFd();
    }

    /** Called when a library thumbnail is selected in Rust. */
    public void openRawLibraryDocument(String uriText, String displayName) {
        new Thread(
                () -> {
                    try {
                        materializeLibraryRaw(Uri.parse(uriText), displayName);
                    } catch (Exception error) {
                        nativeOnFilePicked("", displayName, uriText, error.toString(), false);
                    }
                },
                "AuRaw library open").start();
    }

    /**
     * Copies an existing visible sibling sidecar into private cache. Rust calls
     * this only from its decode worker, then removes the returned cache file.
     * An empty result means that the RAW has no sidecar yet.
     */
    public String materializeRawSidecar(String rawUriText, String displayName) throws Exception {
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        Uri sidecarUri;
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            ArrayList<Uri> matches = scopedSidecarUris(displayName);
            sidecarUri = matches.isEmpty() ? null : matches.get(0);
        } else {
            File sidecar = new File(legacyRawLibraryDirectory(), sidecarDisplayName(displayName));
            sidecarUri = sidecar.isFile() ? Uri.fromFile(sidecar) : null;
        }
        if (sidecarUri == null) {
            return "";
        }

        File cached = File.createTempFile("auraw-sidecar-", ".auraw", getCacheDir());
        boolean completed = false;
        try {
            try (InputStream input = openLibraryInput(sidecarUri);
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
    public String createRawSidecarCache() throws Exception {
        return File.createTempFile("auraw-sidecar-", ".auraw", getCacheDir()).getAbsolutePath();
    }

    /**
     * Publishes a completed staging file beside its RAW. API 29+ uses a fresh
     * pending MediaStore row, so readers never observe partial JSON. Android
     * 8–9 copies to a sibling temporary file and atomically renames it.
     */
    public String publishRawSidecar(
            String cachedPath,
            String rawUriText,
            String displayName) throws Exception {
        File cached = new File(cachedPath);
        if (!cached.isFile() || cached.length() > MAX_SIDECAR_BYTES) {
            throw new IllegalStateException("AuRaw sidecar staging file is missing or too large");
        }
        Uri rawUri = Uri.parse(rawUriText);
        verifyRawLibraryIdentity(rawUri, displayName);
        return Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q
                ? publishRawSidecarScoped(cached, displayName)
                : publishRawSidecarLegacy(cached, displayName);
    }

    private String publishRawSidecarScoped(File cached, String rawDisplayName) throws Exception {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Scoped sidecars require Android 10 or newer");
        }
        ContentResolver resolver = getContentResolver();
        String displayName = sidecarDisplayName(rawDisplayName);
        String stagedName = sidecarStagePrefix(rawDisplayName)
                + Long.toUnsignedString(System.nanoTime());
        ArrayList<Uri> oldSidecars = scopedSidecarUris(rawDisplayName);
        ContentValues values = new ContentValues();
        values.put(MediaStore.Downloads.DISPLAY_NAME, stagedName);
        // MediaProvider may rewrite unknown extensions to match a specific
        // MIME type (for example `.auraw.json`). The unknown binary MIME keeps
        // AuRaw's exact custom filename intact.
        values.put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream");
        values.put(MediaStore.Downloads.RELATIVE_PATH, RAW_LIBRARY_RELATIVE_PATH);
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
                    removedOldRows &= resolver.delete(oldSidecar, null, null) > 0;
                }
            }
            if (!removedOldRows) {
                return RAW_LIBRARY_RELATIVE_PATH + stagedName;
            }
            values.clear();
            values.put(MediaStore.Downloads.DISPLAY_NAME, displayName);
            if (resolver.update(destination, values, null, null) <= 0) {
                return RAW_LIBRARY_RELATIVE_PATH + stagedName;
            }
            String actualName = queryStoredDisplayName(destination);
            if (!displayName.equals(actualName)) {
                values.clear();
                values.put(MediaStore.Downloads.DISPLAY_NAME, stagedName);
                resolver.update(destination, values, null, null);
                return RAW_LIBRARY_RELATIVE_PATH + queryStoredDisplayName(destination);
            }
            return RAW_LIBRARY_RELATIVE_PATH + displayName;
        } finally {
            // Once the staged row is published it is a complete, discoverable
            // recovery generation. Preserve it if final renaming fails.
            if (!contentPublished) {
                resolver.delete(destination, null, null);
            }
        }
    }

    private String publishRawSidecarLegacy(File cached, String rawDisplayName) throws Exception {
        File directory = legacyRawLibraryDirectory();
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

    private ArrayList<Uri> scopedSidecarUris(String rawDisplayName) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Scoped sidecars require Android 10 or newer");
        }
        ArrayList<Uri> result = new ArrayList<>();
        String displayName = sidecarDisplayName(rawDisplayName);
        String stagedPrefix = sidecarStagePrefix(rawDisplayName);
        String[] projection = {
                MediaStore.Downloads._ID,
                MediaStore.Downloads.DISPLAY_NAME
        };
        String selection = MediaStore.Downloads.RELATIVE_PATH + "=? AND "
                + MediaStore.Downloads.OWNER_PACKAGE_NAME + "=? AND "
                + MediaStore.Downloads.IS_PENDING + "=0 AND ("
                + MediaStore.Downloads.DISPLAY_NAME + "=? OR "
                + MediaStore.Downloads.DISPLAY_NAME + " LIKE ? ESCAPE '\\')";
        String[] args = {
                RAW_LIBRARY_RELATIVE_PATH,
                getPackageName(),
                displayName,
                escapeLike(stagedPrefix) + "%"
        };
        try (Cursor cursor = getContentResolver().query(
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
                if (displayName.equals(foundName) || foundName.startsWith(stagedPrefix)) {
                    result.add(ContentUris.withAppendedId(
                            MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                            cursor.getLong(idColumn)));
                }
            }
        }
        return result;
    }

    private void verifyRawLibraryIdentity(Uri rawUri, String expectedDisplayName) throws Exception {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            String[] projection = {
                    MediaStore.Downloads.DISPLAY_NAME,
                    MediaStore.Downloads.RELATIVE_PATH,
                    MediaStore.Downloads.OWNER_PACKAGE_NAME
            };
            try (Cursor cursor = getContentResolver().query(rawUri, projection, null, null, null)) {
                if (cursor == null || !cursor.moveToFirst()) {
                    throw new IllegalArgumentException("The RAW is no longer in the AuRaw library");
                }
                String name = cursor.getString(cursor.getColumnIndexOrThrow(
                        MediaStore.Downloads.DISPLAY_NAME));
                String relativePath = cursor.getString(cursor.getColumnIndexOrThrow(
                        MediaStore.Downloads.RELATIVE_PATH));
                String owner = cursor.getString(cursor.getColumnIndexOrThrow(
                        MediaStore.Downloads.OWNER_PACKAGE_NAME));
                if (!expectedDisplayName.equals(name)
                        || !RAW_LIBRARY_RELATIVE_PATH.equals(relativePath)
                        || !getPackageName().equals(owner)) {
                    throw new IllegalArgumentException("The RAW is outside AuRaw's library");
                }
            }
            return;
        }

        if (!ContentResolver.SCHEME_FILE.equals(rawUri.getScheme())) {
            throw new IllegalArgumentException("The legacy RAW library URI is invalid");
        }
        File raw = new File(rawUri.getPath()).getCanonicalFile();
        File directory = legacyRawLibraryDirectory().getCanonicalFile();
        if (!expectedDisplayName.equals(raw.getName())
                || raw.getParentFile() == null
                || !directory.equals(raw.getParentFile())) {
            throw new IllegalArgumentException("The RAW is outside AuRaw's library");
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
        return Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q
                ? storeRawScoped(source, requestedName)
                : storeRawLegacy(source, requestedName);
    }

    private StoredRaw storeRawScoped(Uri source, String requestedName) throws Exception {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Scoped RAW storage requires Android 10 or newer");
        }
        ContentResolver resolver = getContentResolver();
        String displayName = uniqueScopedRawName(safeRawName(requestedName));
        ContentValues values = new ContentValues();
        values.put(MediaStore.Downloads.DISPLAY_NAME, displayName);
        // Providers frequently mislabel camera RAWs as JPEG/TIFF. A specific
        // MIME can make MediaProvider rewrite CR3/NEF/etc extensions, so keep
        // the imported filename authoritative.
        values.put(MediaStore.Downloads.MIME_TYPE, "application/octet-stream");
        values.put(MediaStore.Downloads.RELATIVE_PATH, RAW_LIBRARY_RELATIVE_PATH);
        values.put(MediaStore.Downloads.IS_PENDING, 1);

        Uri destination = resolver.insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values);
        if (destination == null) {
            throw new IllegalStateException("Android MediaStore could not create the RAW");
        }
        boolean published = false;
        try {
            try (InputStream input = resolver.openInputStream(source);
                 OutputStream output = resolver.openOutputStream(destination, "w")) {
                if (input == null || output == null) {
                    throw new IllegalStateException("Android storage returned no RAW stream");
                }
                copy(input, output, MAX_RAW_IMPORT_BYTES);
            }
            values.clear();
            values.put(MediaStore.Downloads.IS_PENDING, 0);
            if (resolver.update(destination, values, null, null) <= 0) {
                throw new IllegalStateException("Android MediaStore could not publish the RAW");
            }
            String storedDisplayName = queryStoredDisplayName(destination);
            published = true;
            return new StoredRaw(destination, storedDisplayName);
        } finally {
            if (!published) {
                resolver.delete(destination, null, null);
            }
        }
    }

    private StoredRaw storeRawLegacy(Uri source, String requestedName) throws Exception {
        File directory = legacyRawLibraryDirectory();
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create " + directory);
        }
        File destination = uniqueRawFile(directory, safeRawName(requestedName));
        File partial = uniqueRawFile(
                directory,
                ".auraw-import-" + destination.getName() + ".part");
        boolean completed = false;
        try {
            try (InputStream input = getContentResolver().openInputStream(source);
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

    private void materializeLibraryRaw(Uri source, String displayName) throws Exception {
        File cached = File.createTempFile("auraw-library-", suffixFor(displayName), getCacheDir());
        boolean completed = false;
        try {
            try (InputStream input = openLibraryInput(source);
                 OutputStream output = new FileOutputStream(cached)) {
                if (input == null) {
                    throw new IllegalStateException("Android storage returned no RAW stream");
                }
                copy(input, output, MAX_RAW_IMPORT_BYTES);
            }
            completed = true;
            nativeOnFilePicked(
                    cached.getAbsolutePath(), displayName, source.toString(), "", true);
        } finally {
            if (!completed && !cached.delete() && cached.exists()) {
                cached.deleteOnExit();
            }
        }
    }

    private InputStream openLibraryInput(Uri uri) throws Exception {
        if (ContentResolver.SCHEME_FILE.equals(uri.getScheme())) {
            return new FileInputStream(new File(uri.getPath()));
        }
        return getContentResolver().openInputStream(uri);
    }

    private String listScopedRawLibrary() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Scoped RAW storage requires Android 10 or newer");
        }
        StringBuilder result = new StringBuilder();
        String[] projection = {
                MediaStore.Downloads._ID,
                MediaStore.Downloads.DISPLAY_NAME,
                MediaStore.Downloads.SIZE,
                MediaStore.Downloads.DATE_MODIFIED
        };
        String selection = MediaStore.Downloads.RELATIVE_PATH + "=? AND "
                + MediaStore.Downloads.OWNER_PACKAGE_NAME + "=? AND "
                + MediaStore.Downloads.IS_PENDING + "=0";
        String[] selectionArgs = {RAW_LIBRARY_RELATIVE_PATH, getPackageName()};
        try (Cursor cursor = getContentResolver().query(
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
            int added = 0;
            // Return one sentinel record beyond the UI limit so Rust can
            // distinguish exactly 20,000 files from a truncated collection.
            while (added <= MAX_RAW_LIBRARY_FILES && cursor.moveToNext()) {
                String name = cursor.getString(nameColumn);
                if (!isRawName(name)) {
                    continue;
                }
                Uri uri = ContentUris.withAppendedId(
                        MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                        cursor.getLong(idColumn));
                appendLibraryRecord(
                        result,
                        uri.toString(),
                        name,
                        RAW_LIBRARY_RELATIVE_PATH + name,
                        Math.max(0, cursor.getLong(sizeColumn)),
                        Math.max(0, cursor.getLong(modifiedColumn)));
                added++;
            }
        }
        return result.toString();
    }

    private String listLegacyRawLibrary() {
        StringBuilder result = new StringBuilder();
        File[] files = legacyRawLibraryDirectory().listFiles();
        if (files == null) {
            return "";
        }
        Arrays.sort(files, (left, right) -> Long.compare(right.lastModified(), left.lastModified()));
        int added = 0;
        for (File file : files) {
            // Return one sentinel record beyond the UI limit; Rust displays
            // only the first MAX_RAW_LIBRARY_FILES entries.
            if (added > MAX_RAW_LIBRARY_FILES) {
                break;
            }
            if (!file.isFile() || !isRawName(file.getName())) {
                continue;
            }
            appendLibraryRecord(
                    result,
                    Uri.fromFile(file).toString(),
                    file.getName(),
                    file.getAbsolutePath(),
                    Math.max(0, file.length()),
                    Math.max(0, file.lastModified() / 1000));
            added++;
        }
        return result.toString();
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

    private String uniqueScopedRawName(String requestedName) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Scoped RAW storage requires Android 10 or newer");
        }
        if (!scopedRawNameExists(requestedName)) {
            return requestedName;
        }
        int dot = requestedName.lastIndexOf('.');
        String stem = dot > 0 ? requestedName.substring(0, dot) : requestedName;
        String suffix = dot > 0 ? requestedName.substring(dot) : "";
        for (int index = 1; ; index++) {
            String candidate = stem + "-" + index + suffix;
            if (!scopedRawNameExists(candidate)) {
                return candidate;
            }
        }
    }

    private boolean scopedRawNameExists(String displayName) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            throw new IllegalStateException("Scoped RAW storage requires Android 10 or newer");
        }
        String[] projection = {MediaStore.Downloads._ID};
        // Intentionally include pending rows so a crash-left import still
        // reserves its display name instead of being overwritten/reused.
        String selection = MediaStore.Downloads.RELATIVE_PATH + "=? AND "
                + MediaStore.Downloads.DISPLAY_NAME + "=? AND "
                + MediaStore.Downloads.OWNER_PACKAGE_NAME + "=?";
        String[] args = {RAW_LIBRARY_RELATIVE_PATH, displayName, getPackageName()};
        try (Cursor cursor = getContentResolver().query(
                MediaStore.Downloads.EXTERNAL_CONTENT_URI,
                projection,
                selection,
                args,
                null)) {
            return cursor != null && cursor.moveToFirst();
        }
    }

    private String queryStoredDisplayName(Uri uri) {
        String[] projection = {MediaStore.Downloads.DISPLAY_NAME};
        try (Cursor cursor = getContentResolver().query(uri, projection, null, null, null)) {
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

    private File legacyRawLibraryDirectory() {
        File downloads = getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS);
        if (downloads == null) {
            downloads = new File(getFilesDir(), Environment.DIRECTORY_DOWNLOADS);
        }
        return new File(downloads, "AuRaw");
    }

    private void deleteStoredRaw(Uri uri) {
        try {
            if (ContentResolver.SCHEME_FILE.equals(uri.getScheme())) {
                new File(uri.getPath()).delete();
            } else {
                getContentResolver().delete(uri, null, null);
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

    private static final class StoredRaw {
        final Uri uri;
        final String displayName;

        StoredRaw(Uri uri, String displayName) {
            this.uri = uri;
            this.displayName = displayName;
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
