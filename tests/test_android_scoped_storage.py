from __future__ import annotations

import shutil
import subprocess
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[1]
CONTRACT = ROOT / "android/app/src/main/java/de/duecki/auraw/AndroidStorageContract.java"
HARNESS = r'''
package de.duecki.auraw;

import java.io.File;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.Arrays;

public final class AndroidStorageContractHarness {
    private static void require(boolean condition, String message) {
        if (!condition) throw new AssertionError(message);
    }

    public static void main(String[] args) throws Exception {
        File root = Files.createTempDirectory("auraw-storage-contract").toFile();
        File media = new File(root, "media");
        File library = AndroidStorageContract.rawLibraryDirectory(media);
        require(library.getName().equals(".library"), "hidden library name");
        require(AndroidStorageContract.noMediaMarker(library).getName().equals(".nomedia"), "nomedia marker");

        require(AndroidStorageContract.safeRawName("a/b\\c.dng").equals("a_b_c.dng"), "safe RAW name");
        require(AndroidStorageContract.isRawName("capture.DNG"), "DNG accepted");
        require(AndroidStorageContract.isRawName("capture.raf"), "RAF accepted");
        require(!AndroidStorageContract.isRawName("capture.jpg"), "JPEG rejected");
        require(AndroidStorageContract.sidecarDisplayName("capture.dng").equals("capture.dng.auraw"), "sidecar name");
        require(AndroidStorageContract.importPartialName("capture.dng").equals(".auraw-import-capture.dng.part"), "partial name");
        require(AndroidStorageContract.sidecarStagePrefix("capture.dng")
                .equals(AndroidStorageContract.sidecarStagePrefix("capture.dng")), "stable stage prefix");

        File legacy = new File(media, "legacy");
        require(library.mkdirs() && legacy.mkdirs(), "test directories");
        File currentRaw = new File(library, "capture.dng");
        File legacyRaw = new File(legacy, "old.nef");
        Files.write(currentRaw.toPath(), new byte[] {1});
        Files.write(legacyRaw.toPath(), new byte[] {2});
        require(AndroidStorageContract.isAllowedRawFile(currentRaw, "capture.dng", library, legacy), "current identity");
        require(AndroidStorageContract.isAllowedRawFile(legacyRaw, "old.nef", library, legacy), "legacy identity");
        require(!AndroidStorageContract.isAllowedRawFile(currentRaw, "renamed.dng", library, legacy), "name mismatch");
        File outside = new File(root, "capture.dng");
        Files.write(outside.toPath(), new byte[] {3});
        require(!AndroidStorageContract.isAllowedRawFile(outside, "capture.dng", library, legacy), "outside identity");

        File staged = new File(root, "staged.auraw");
        byte[] payload = "new-sidecar".getBytes(StandardCharsets.UTF_8);
        Files.write(staged.toPath(), payload);
        File destination = new File(AndroidStorageContract.publishSidecarAtomically(
                staged, library, "capture.dng", payload.length));
        require(destination.getParentFile().getCanonicalFile().equals(library.getCanonicalFile()), "sibling publish");
        require(Arrays.equals(Files.readAllBytes(destination.toPath()), payload), "published bytes");
        File[] partials = library.listFiles((dir, name) -> name.endsWith(".part"));
        require(partials != null && partials.length == 0, "no partial files");

        byte[] oldPayload = "old-sidecar".getBytes(StandardCharsets.UTF_8);
        Files.write(destination.toPath(), oldPayload);
        File oversized = new File(root, "oversized.auraw");
        Files.write(oversized.toPath(), new byte[] {1, 2, 3, 4});
        boolean rejected = false;
        try {
            AndroidStorageContract.publishSidecarAtomically(oversized, library, "capture.dng", 3);
        } catch (IllegalStateException expected) {
            rejected = true;
        }
        require(rejected, "oversized sidecar rejected");
        require(Arrays.equals(Files.readAllBytes(destination.toPath()), oldPayload), "existing sidecar preserved");

        require(AndroidStorageContract.isAllowedLegacyMediaStoreRow(
                42, 42, "old.nef", "old.nef", "Download/AuRaw/", "Download/AuRaw/",
                "de.duecki.auraw", "de.duecki.auraw", 0, false), "legacy MediaStore row");
        require(!AndroidStorageContract.isAllowedLegacyMediaStoreRow(
                42, 42, "old.nef", "old.nef", "Download/AuRaw/", "Download/AuRaw/",
                "de.duecki.auraw", "other.owner", 0, false), "foreign MediaStore owner");
        require(!AndroidStorageContract.isAllowedLegacyMediaStoreRow(
                42, 42, "old.nef", "old.nef", "Download/AuRaw/", "Download/AuRaw/",
                "de.duecki.auraw", "de.duecki.auraw", 1, false), "pending MediaStore row");

        AndroidStorageContract.deleteSidecar(library, "capture.dng");
        require(!destination.exists(), "complete sidecar deleted");

        File legacyMoveSource = new File(root, "legacy-move.raw");
        File legacyMoveDestination = new File(library, "legacy-move.raw");
        byte[] migratedPayload = "legacy raw".getBytes(StandardCharsets.UTF_8);
        Files.write(legacyMoveSource.toPath(), migratedPayload);
        AndroidStorageContract.moveOrCopyLegacyFile(
                legacyMoveSource, legacyMoveDestination, migratedPayload.length);
        require(!legacyMoveSource.exists(), "legacy source removed after migration");
        require(Arrays.equals(
                Files.readAllBytes(legacyMoveDestination.toPath()), migratedPayload),
                "legacy payload migrated");

        require(AndroidStorageContract.exportRelativePath("Pictures").equals("Pictures/AuRaw"), "export folder");
        require(AndroidStorageContract.exportLocation("Pictures", "edit.png").equals("Pictures/AuRaw/edit.png"), "export location");
        require(AndroidStorageContract.normalizeExportMimeType("IMAGE/JPEG").equals("image/jpeg"), "JPEG MIME");
        require(AndroidStorageContract.normalizeExportMimeType("image/webp").equals("image/png"), "PNG fallback");
        require(AndroidStorageContract.safeImageName("summer edit", "image/png").equals("summer_edit.png"), "safe image name");
        System.out.println("storage contract passed");
    }
}
'''


@pytest.fixture(scope="module")
def compiled_contract(tmp_path_factory: pytest.TempPathFactory) -> Path:
    if shutil.which("javac") is None or shutil.which("java") is None:
        pytest.skip("a JDK is required for compiler-backed Android storage tests")
    build = tmp_path_factory.mktemp("android-storage-contract")
    harness = build / "AndroidStorageContractHarness.java"
    harness.write_text(HARNESS, encoding="utf-8")
    completed = subprocess.run(
        ["javac", "-d", str(build), str(CONTRACT), str(harness)],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
    return build


def test_android_storage_contract_behaves_correctly(compiled_contract: Path) -> None:
    completed = subprocess.run(
        ["java", "-cp", str(compiled_contract), "de.duecki.auraw.AndroidStorageContractHarness"],
        cwd=ROOT,
        check=False,
        capture_output=True,
        text=True,
    )
    assert completed.returncode == 0, completed.stdout + completed.stderr
    assert completed.stdout.strip() == "storage contract passed"
