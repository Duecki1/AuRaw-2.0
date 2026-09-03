package de.duecki.calibraw;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.util.Arrays;
import java.util.HashSet;
import java.util.Locale;
import java.util.Set;

final class AndroidStorageContract {
    static final String RAW_LIBRARY_DIRECTORY_NAME = ".library";
    static final int MAX_RAW_NAME_BYTES = 220;
    static final int MAX_EXPORT_NAME_BYTES = 240;

    private static final Set<String> RAW_SUFFIXES = new HashSet<>(Arrays.asList(
            "3fr", "ari", "arw", "bay", "bmq", "cap", "cine", "cr2", "cr3", "crw",
            "cs1", "dc2", "dcr", "dcs", "dng", "drf", "eip", "erf", "fff", "gpr",
            "iiq", "k25", "kc2", "kdc", "mdc", "mef", "mos", "mrw", "nef", "nrw",
            "obm", "orf", "pef", "ptx", "pxn", "qtk", "r3d", "raf", "raw", "rdc",
            "rw2", "rwl", "rwz", "sr2", "srf", "srw", "sti", "tif", "tiff", "x3f"));

    private AndroidStorageContract() {}

    static File rawLibraryDirectory(File externalMediaRoot) {
        return new File(externalMediaRoot, RAW_LIBRARY_DIRECTORY_NAME);
    }

    static File noMediaMarker(File rawLibraryDirectory) {
        return new File(rawLibraryDirectory, ".nomedia");
    }

    static String safeFolderName(String requestedName) {
        String name = requestedName == null ? "" : requestedName.trim();
        if (name.isEmpty()
                || ".".equals(name)
                || "..".equals(name)
                || name.startsWith(".")
                || name.indexOf('/') >= 0
                || name.indexOf('\\') >= 0
                || name.indexOf('\0') >= 0
                || name.getBytes(StandardCharsets.UTF_8).length > 240) {
            throw new IllegalArgumentException("Enter a safe folder name");
        }
        return name;
    }

    static File libraryFolder(File canonicalLibrary, String relativePath) throws Exception {
        String relative = relativePath == null ? "" : relativePath.trim();
        File root = canonicalLibrary.getCanonicalFile();
        if (relative.isEmpty()) {
            return root;
        }
        if (relative.startsWith("/") || relative.startsWith("\\")) {
            throw new IllegalArgumentException("The library folder path is invalid");
        }
        File current = root;
        for (String component : relative.replace('\\', '/').split("/", -1)) {
            if (component.isEmpty() || !safeFolderName(component).equals(component)) {
                throw new IllegalArgumentException("The library folder path is invalid");
            }
            current = new File(current, component);
        }
        File resolved = current.getCanonicalFile();
        if (!resolved.toPath().startsWith(root.toPath())) {
            throw new IllegalArgumentException("The library folder is outside CalibRaw's library");
        }
        return resolved;
    }

    static String relativeLibraryFolder(File canonicalLibrary, File folder) throws Exception {
        File root = canonicalLibrary.getCanonicalFile();
        File resolved = folder.getCanonicalFile();
        if (!resolved.toPath().startsWith(root.toPath())) {
            throw new IllegalArgumentException("The folder is outside CalibRaw's library");
        }
        return root.toPath().relativize(resolved.toPath()).toString().replace(File.separatorChar, '/');
    }

    static boolean isAllowedRawFile(File raw, String expectedDisplayName, File canonicalLibrary)
            throws Exception {
        if (raw == null || expectedDisplayName == null) {
            return false;
        }
        File canonicalRaw = raw.getCanonicalFile();
        File parent = canonicalRaw.getParentFile();
        File canonicalLibraryRoot = canonicalLibrary.getCanonicalFile();
        return expectedDisplayName.equals(canonicalRaw.getName())
                && parent != null
                && parent.toPath().startsWith(canonicalLibraryRoot.toPath());
    }

    static String safeRawName(String requestedName) {
        return truncateUtf8PreservingExtension(sanitizeRawName(requestedName), MAX_RAW_NAME_BYTES);
    }

    private static String sanitizeRawName(String requestedName) {
        String name = requestedName == null ? "imported.raw" : requestedName.trim();
        name = name.replace('/', '_').replace('\\', '_').replace('\0', '_');
        return name.isEmpty() ? "imported.raw" : name;
    }

    static boolean isRawName(String displayName) {
        if (displayName == null) {
            return false;
        }
        int dot = displayName.lastIndexOf('.');
        return dot >= 0 && dot < displayName.length() - 1
                && RAW_SUFFIXES.contains(displayName.substring(dot + 1).toLowerCase(Locale.ROOT));
    }

    static String sidecarDisplayName(String rawDisplayName) {
        String name = sanitizeRawName(rawDisplayName);
        if (!name.equals(rawDisplayName)
                || name.getBytes(StandardCharsets.UTF_8).length > 240) {
            throw new IllegalArgumentException("The RAW name cannot be used for a sidecar");
        }
        return name + ".calibraw";
    }

    static String importPartialName(String destinationName) {
        return ".calibraw-import-" + destinationName + ".part";
    }

    static boolean isLibraryTemporaryFileName(String name) {
        if (name == null || !name.endsWith(".part")) {
            return false;
        }
        return name.startsWith(".calibraw-import-")
                || name.startsWith(".calibraw-sidecar-");
    }

    static String exportRelativePath(String picturesDirectory) {
        return picturesDirectory + "/CalibRaw";
    }

    static String exportLocation(String picturesDirectory, String displayName) {
        return exportRelativePath(picturesDirectory) + "/" + displayName;
    }

    static String normalizeExportMimeType(String mimeType) {
        return "image/jpeg".equalsIgnoreCase(mimeType) ? "image/jpeg" : "image/png";
    }

    static String safeImageName(String requestedName, String mimeType) {
        boolean jpeg = "image/jpeg".equalsIgnoreCase(mimeType);
        String extension = jpeg ? ".jpg" : ".png";
        String fallback = jpeg ? "CalibRaw-export.jpg" : "CalibRaw-export.png";
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
        return truncateUtf8PreservingExtension(name, MAX_EXPORT_NAME_BYTES);
    }

    static String truncateUtf8PreservingExtension(String name, int maximumBytes) {
        if (name.getBytes(StandardCharsets.UTF_8).length <= maximumBytes) {
            return name;
        }
        int dot = name.lastIndexOf('.');
        if (dot <= 0) {
            return truncateUtf8(name, maximumBytes);
        }
        String extension = name.substring(dot);
        int extensionBytes = extension.getBytes(StandardCharsets.UTF_8).length;
        if (extensionBytes >= maximumBytes) {
            return truncateUtf8(name, maximumBytes);
        }
        return truncateUtf8(name.substring(0, dot), maximumBytes - extensionBytes) + extension;
    }

    private static String truncateUtf8(String value, int maximumBytes) {
        StringBuilder truncated = new StringBuilder(value.length());
        int bytes = 0;
        for (int offset = 0; offset < value.length(); ) {
            int codePoint = value.codePointAt(offset);
            String character = new String(Character.toChars(codePoint));
            int characterBytes = character.getBytes(StandardCharsets.UTF_8).length;
            if (bytes + characterBytes > maximumBytes) {
                break;
            }
            truncated.append(character);
            bytes += characterBytes;
            offset += Character.charCount(codePoint);
        }
        return truncated.toString();
    }

    static void deleteSidecar(File directory, String rawDisplayName) {
        File sidecar = new File(directory, sidecarDisplayName(rawDisplayName));
        if (sidecar.exists() && !sidecar.delete()) {
            throw new IllegalStateException("Could not delete the RAW sidecar");
        }
    }

    static String publishSidecarAtomically(
            File cached,
            File directory,
            String rawDisplayName,
            long maximumBytes) throws Exception {
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create " + directory);
        }
        File destination = new File(directory, sidecarDisplayName(rawDisplayName));
        File temporary = File.createTempFile(".calibraw-sidecar-", ".part", directory);
        boolean published = false;
        try {
            try (FileInputStream input = new FileInputStream(cached);
                 FileOutputStream output = new FileOutputStream(temporary)) {
                BoundedStreams.copy(
                        input,
                        output,
                        maximumBytes,
                        "File exceeds the allowed size");
                output.getFD().sync();
            }
            try {
                Files.move(
                        temporary.toPath(),
                        destination.toPath(),
                        StandardCopyOption.ATOMIC_MOVE,
                        StandardCopyOption.REPLACE_EXISTING);
            } catch (AtomicMoveNotSupportedException unsupported) {
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

}
