package de.duecki.auraw;

import java.io.File;
import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.AtomicMoveNotSupportedException;
import java.nio.file.Files;
import java.nio.file.StandardCopyOption;
import java.security.MessageDigest;
import java.util.Arrays;
import java.util.HashSet;
import java.util.Locale;
import java.util.Set;

/** Platform-independent storage rules shared by Android production code and JVM tests. */
final class AndroidStorageContract {
    static final String RAW_LIBRARY_DIRECTORY_NAME = ".library";

    private static final Set<String> RAW_SUFFIXES = new HashSet<>(Arrays.asList(
            "3fr", "ari", "arw", "bay", "bmq", "cap", "cine", "cr2", "cr3", "crw",
            "cs1", "dc2", "dcr", "dcs", "dng", "drf", "eip", "erf", "fff", "gpr",
            "iiq", "k25", "kc2", "kdc", "mdc", "mef", "mos", "mrw", "nef", "nrw",
            "obm", "orf", "pef", "ptx", "pxn", "qtk", "r3d", "raf", "raw", "rdc",
            "rw2", "rwl", "rwz", "sr2", "srf", "srw", "sti", "x3f"));

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

    /** Resolves an AuRaw-owned relative folder without permitting path traversal. */
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
            throw new IllegalArgumentException("The library folder is outside AuRaw's library");
        }
        return resolved;
    }

    static String relativeLibraryFolder(File canonicalLibrary, File folder) throws Exception {
        File root = canonicalLibrary.getCanonicalFile();
        File resolved = folder.getCanonicalFile();
        if (!resolved.toPath().startsWith(root.toPath())) {
            throw new IllegalArgumentException("The folder is outside AuRaw's library");
        }
        return root.toPath().relativize(resolved.toPath()).toString().replace(File.separatorChar, '/');
    }

    static boolean isAllowedRawFile(
            File raw,
            String expectedDisplayName,
            File canonicalLibrary,
            File legacyRoot) throws Exception {
        if (raw == null || expectedDisplayName == null) {
            return false;
        }
        File canonicalRaw = raw.getCanonicalFile();
        File parent = canonicalRaw.getParentFile();
        File canonicalLibraryRoot = canonicalLibrary.getCanonicalFile();
        return expectedDisplayName.equals(canonicalRaw.getName())
                && parent != null
                && (parent.toPath().startsWith(canonicalLibraryRoot.toPath())
                        || legacyRoot.getCanonicalFile().equals(parent));
    }

    static String safeRawName(String requestedName) {
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

    static boolean isAllowedLegacyMediaStoreRow(
            long expectedId,
            long storedId,
            String expectedDisplayName,
            String storedDisplayName,
            String expectedRelativePath,
            String storedRelativePath,
            String expectedOwner,
            String storedOwner,
            int pending,
            boolean hasAdditionalRow) {
        return expectedId >= 0
                && expectedId == storedId
                && expectedDisplayName != null
                && expectedDisplayName.equals(storedDisplayName)
                && expectedRelativePath.equals(storedRelativePath)
                && expectedOwner.equals(storedOwner)
                && pending == 0
                && !hasAdditionalRow;
    }

    static String sidecarDisplayName(String rawDisplayName) {
        String name = safeRawName(rawDisplayName);
        if (!name.equals(rawDisplayName)
                || name.getBytes(StandardCharsets.UTF_8).length > 240) {
            throw new IllegalArgumentException("The RAW name cannot be used for a sidecar");
        }
        return name + ".auraw";
    }

    static String sidecarStagePrefix(String rawDisplayName) {
        try {
            byte[] digest = MessageDigest.getInstance("SHA-256").digest(
                    rawDisplayName.getBytes(StandardCharsets.UTF_8));
            StringBuilder prefix = new StringBuilder(".auraw-stage-");
            for (int index = 0; index < 16; index++) {
                prefix.append(String.format(Locale.ROOT, "%02x", digest[index] & 0xff));
            }
            return prefix.append('-').toString();
        } catch (Exception impossible) {
            return ".auraw-stage-" + Integer.toUnsignedString(rawDisplayName.hashCode(), 16) + '-';
        }
    }

    static String importPartialName(String destinationName) {
        return ".auraw-import-" + destinationName + ".part";
    }

    static String exportRelativePath(String picturesDirectory) {
        return picturesDirectory + "/AuRaw";
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

    static void deleteSidecar(File directory, String rawDisplayName) {
        File sidecar = new File(directory, sidecarDisplayName(rawDisplayName));
        if (sidecar.exists() && !sidecar.delete()) {
            throw new IllegalStateException("Could not delete the RAW sidecar");
        }
    }

    static void moveOrCopyLegacyFile(File source, File destination, long maximumBytes)
            throws Exception {
        try {
            Files.move(source.toPath(), destination.toPath(), StandardCopyOption.ATOMIC_MOVE);
            return;
        } catch (AtomicMoveNotSupportedException unsupported) {
            try {
                Files.move(source.toPath(), destination.toPath());
                return;
            } catch (Exception ignored) {
                // Fall through to bounded copy for filesystems without a reliable move.
            }
        } catch (Exception ignored) {
            // Fall through to bounded copy for filesystems without a reliable move.
        }

        File partial = new File(
                destination.getParentFile(), ".auraw-move-" + destination.getName() + ".part");
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

    static String publishSidecarAtomically(
            File cached,
            File directory,
            String rawDisplayName,
            long maximumBytes) throws Exception {
        if (!directory.isDirectory() && !directory.mkdirs()) {
            throw new IllegalStateException("Could not create " + directory);
        }
        File destination = new File(directory, sidecarDisplayName(rawDisplayName));
        File temporary = File.createTempFile(".auraw-sidecar-", ".part", directory);
        boolean published = false;
        try {
            try (FileInputStream input = new FileInputStream(cached);
                 FileOutputStream output = new FileOutputStream(temporary)) {
                copy(input, output, maximumBytes);
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

    private static void copy(InputStream input, OutputStream output, long maximumBytes)
            throws Exception {
        byte[] buffer = new byte[256 * 1024];
        long total = 0L;
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
                if (total >= maximumBytes) {
                    throw new IllegalStateException("File exceeds the allowed size");
                }
                output.write(value);
                total++;
                continue;
            }
            if (total > maximumBytes - count) {
                throw new IllegalStateException("File exceeds the allowed size");
            }
            output.write(buffer, 0, count);
            total += count;
        }
    }
}
