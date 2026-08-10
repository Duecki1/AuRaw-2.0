package de.duecki.auraw;

import static org.junit.Assert.assertArrayEquals;
import static org.junit.Assert.assertEquals;
import static org.junit.Assert.assertFalse;
import static org.junit.Assert.assertTrue;
import static org.junit.Assert.fail;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import org.junit.Rule;
import org.junit.Test;
import org.junit.rules.TemporaryFolder;

public final class AndroidStorageContractTest {
    @Rule public final TemporaryFolder temporaryFolder = new TemporaryFolder();

    @Test
    public void namesAndRawFileIdentityFollowTheStorageContract() throws Exception {
        File root = temporaryFolder.getRoot();
        File media = new File(root, "media");
        File library = AndroidStorageContract.rawLibraryDirectory(media);

        assertEquals(".library", library.getName());
        assertEquals(".nomedia", AndroidStorageContract.noMediaMarker(library).getName());

        assertEquals("a_b_c.dng", AndroidStorageContract.safeRawName("a/b\\c.dng"));
        assertTrue(AndroidStorageContract.isRawName("capture.DNG"));
        assertTrue(AndroidStorageContract.isRawName("capture.raf"));
        assertFalse(AndroidStorageContract.isRawName("capture.jpg"));
        assertEquals("capture.dng.auraw", AndroidStorageContract.sidecarDisplayName("capture.dng"));
        assertEquals(
                ".auraw-import-capture.dng.part",
                AndroidStorageContract.importPartialName("capture.dng"));
        assertEquals(
                AndroidStorageContract.sidecarStagePrefix("capture.dng"),
                AndroidStorageContract.sidecarStagePrefix("capture.dng"));

        File legacy = new File(media, "legacy");
        assertTrue(library.mkdirs());
        assertTrue(legacy.mkdirs());
        File trip = new File(library, "2026/Trip");
        assertTrue(trip.mkdirs());

        assertEquals("Trip", AndroidStorageContract.safeFolderName("Trip"));
        assertEquals(
                trip.getCanonicalFile(),
                AndroidStorageContract.libraryFolder(library, "2026/Trip"));
        assertEquals(
                "2026/Trip",
                AndroidStorageContract.relativeLibraryFolder(library, trip));

        File currentRaw = new File(library, "capture.dng");
        File nestedRaw = new File(trip, "trip.dng");
        File legacyRaw = new File(legacy, "old.nef");
        Files.write(currentRaw.toPath(), new byte[] {1});
        Files.write(nestedRaw.toPath(), new byte[] {2});
        Files.write(legacyRaw.toPath(), new byte[] {2});

        assertTrue(AndroidStorageContract.isAllowedRawFile(
                currentRaw, "capture.dng", library, legacy));
        assertTrue(AndroidStorageContract.isAllowedRawFile(
                legacyRaw, "old.nef", library, legacy));
        assertTrue(AndroidStorageContract.isAllowedRawFile(
                nestedRaw, "trip.dng", library, legacy));
        assertFalse(AndroidStorageContract.isAllowedRawFile(
                currentRaw, "renamed.dng", library, legacy));

        File outside = new File(root, "capture.dng");
        Files.write(outside.toPath(), new byte[] {3});
        assertFalse(AndroidStorageContract.isAllowedRawFile(
                outside, "capture.dng", library, legacy));

        try {
            AndroidStorageContract.libraryFolder(library, "../outside");
            fail("folder traversal should be rejected");
        } catch (IllegalArgumentException expected) {
            // Expected.
        }
    }

    @Test
    public void sidecarsPublishAtomicallyAndRespectTheSizeLimit() throws Exception {
        File root = temporaryFolder.getRoot();
        File library = new File(root, ".library");
        assertTrue(library.mkdirs());

        File staged = new File(root, "staged.auraw");
        byte[] payload = "new-sidecar".getBytes(StandardCharsets.UTF_8);
        Files.write(staged.toPath(), payload);

        File destination = new File(AndroidStorageContract.publishSidecarAtomically(
                staged, library, "capture.dng", payload.length));
        assertEquals(library.getCanonicalFile(), destination.getParentFile().getCanonicalFile());
        assertArrayEquals(payload, Files.readAllBytes(destination.toPath()));

        File[] partials = library.listFiles((directory, name) -> name.endsWith(".part"));
        assertTrue(partials != null && partials.length == 0);

        byte[] oldPayload = "old-sidecar".getBytes(StandardCharsets.UTF_8);
        Files.write(destination.toPath(), oldPayload);
        File oversized = new File(root, "oversized.auraw");
        Files.write(oversized.toPath(), new byte[] {1, 2, 3, 4});

        try {
            AndroidStorageContract.publishSidecarAtomically(
                    oversized, library, "capture.dng", 3);
            fail("oversized sidecar should be rejected");
        } catch (IllegalStateException expected) {
            // Expected: the bounded copy must not replace the existing sidecar.
        }
        assertArrayEquals(oldPayload, Files.readAllBytes(destination.toPath()));

        AndroidStorageContract.deleteSidecar(library, "capture.dng");
        assertFalse(destination.exists());
    }

    @Test
    public void legacyMediaStoreRowsRequireExactIdentityAndOwnership() {
        assertTrue(AndroidStorageContract.isAllowedLegacyMediaStoreRow(
                42,
                42,
                "old.nef",
                "old.nef",
                "Download/AuRaw/",
                "Download/AuRaw/",
                "de.duecki.auraw",
                "de.duecki.auraw",
                0,
                false));
        assertFalse(AndroidStorageContract.isAllowedLegacyMediaStoreRow(
                42,
                42,
                "old.nef",
                "old.nef",
                "Download/AuRaw/",
                "Download/AuRaw/",
                "de.duecki.auraw",
                "other.owner",
                0,
                false));
        assertFalse(AndroidStorageContract.isAllowedLegacyMediaStoreRow(
                42,
                42,
                "old.nef",
                "old.nef",
                "Download/AuRaw/",
                "Download/AuRaw/",
                "de.duecki.auraw",
                "de.duecki.auraw",
                1,
                false));
    }

    @Test
    public void legacyMigrationAndExportNamingFollowTheStorageContract() throws Exception {
        File root = temporaryFolder.getRoot();
        File library = new File(root, ".library");
        assertTrue(library.mkdirs());

        File legacyMoveSource = new File(root, "legacy-move.raw");
        File legacyMoveDestination = new File(library, "legacy-move.raw");
        byte[] migratedPayload = "legacy raw".getBytes(StandardCharsets.UTF_8);
        Files.write(legacyMoveSource.toPath(), migratedPayload);

        AndroidStorageContract.moveOrCopyLegacyFile(
                legacyMoveSource, legacyMoveDestination, migratedPayload.length);
        assertFalse(legacyMoveSource.exists());
        assertArrayEquals(migratedPayload, Files.readAllBytes(legacyMoveDestination.toPath()));

        assertEquals("Pictures/AuRaw", AndroidStorageContract.exportRelativePath("Pictures"));
        assertEquals(
                "Pictures/AuRaw/edit.png",
                AndroidStorageContract.exportLocation("Pictures", "edit.png"));
        assertEquals("image/jpeg", AndroidStorageContract.normalizeExportMimeType("IMAGE/JPEG"));
        assertEquals("image/png", AndroidStorageContract.normalizeExportMimeType("image/webp"));
        assertEquals(
                "summer_edit.png",
                AndroidStorageContract.safeImageName("summer edit", "image/png"));
    }
}
