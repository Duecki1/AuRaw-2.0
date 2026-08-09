package de.duecki.auraw;

import android.app.NativeActivity;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.os.Build;
import android.os.Bundle;
import android.graphics.Insets;
import android.view.View;
import android.view.WindowInsets;
import android.widget.Toast;
import android.window.OnBackInvokedDispatcher;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;

/** Thin Android/JNI bridge. Stateful storage work lives in focused delegates. */
public final class AuRawActivity extends NativeActivity {
    private static final int OPEN_RAW_DOCUMENT = 1001;
    private static final int OPEN_CAMERA_PROFILE_FOLDER = 1003;
    private static final int OPEN_CLOUD_RAW_DOCUMENT = 1004;

    private StorageManager storageManager;
    private ProfileImporter profileImporter;
    private ExportPublisher exportPublisher;
    private TaskNotificationController taskNotificationController;

    static {
        System.loadLibrary("auraw");
    }

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        // NativeActivity may start the Rust thread during creation. Install the
        // notification bridge before the other delegates so early task updates
        // cannot observe a partially initialized Activity.
        taskNotificationController = new TaskNotificationController(this);
        storageManager = new StorageManager(this, new StorageManager.Callbacks() {
            @Override
            public void onFilePicked(
                    String cachedPath,
                    String displayName,
                    String libraryUri,
                    String error,
                    boolean temporary) {
                nativeOnFilePicked(cachedPath, displayName, libraryUri, error, temporary);
            }

            @Override
            public void onFilePickedFd(int fd, String displayName, String libraryUri, String error) {
                nativeOnFilePickedFd(fd, displayName, libraryUri, error);
            }

            @Override
            public void onImportBatchFinished(int importedCount, int failedCount, String errors) {
                nativeOnImportBatchFinished(importedCount, failedCount, errors);
            }

            @Override
            public void onCloudFileSelected(String uri, String displayName, long bytes) {
                nativeOnCloudFileSelected(uri, displayName, bytes);
            }

            @Override
            public void onCloudSelectionFinished(int failedCount, String errors) {
                nativeOnCloudSelectionFinished(failedCount, errors);
            }
        });
        profileImporter = new ProfileImporter(this, new ProfileImporter.Callbacks() {
            @Override
            public void onImportStarted(String displayName) {
                nativeOnCameraProfileFolderImportStarted(displayName);
            }

            @Override
            public void onFolderPicked(
                    String cachedPath, String displayName, int profileCount, String error) {
                nativeOnCameraProfileFolderPicked(cachedPath, displayName, profileCount, error);
            }
        });
        exportPublisher = new ExportPublisher(this, AuRawActivity::nativeOnExportPublished);

        configureSystemBarsAndInsets();
        storageManager.scavengeTemporaryRawFiles();
        storageManager.startLegacyRawStorageMigration();
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
                    OnBackInvokedDispatcher.PRIORITY_DEFAULT,
                    () -> {
                        if (!nativeOnBackRequested()) {
                            finish();
                        }
                    });
        }
    }

    @SuppressWarnings("deprecation") // Required on API 30–34; API 35+ is always edge-to-edge.
    private void configureSystemBarsAndInsets() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.VANILLA_ICE_CREAM) {
                getWindow().setDecorFitsSystemWindows(false);
            }
            View decorView = getWindow().getDecorView();
            decorView.setOnApplyWindowInsetsListener((view, windowInsets) -> {
                Insets safeInsets = windowInsets.getInsets(
                        WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
                nativeOnSystemInsetsChanged(
                        safeInsets.left, safeInsets.top, safeInsets.right, safeInsets.bottom);
                return windowInsets;
            });
            decorView.requestApplyInsets();
        } else {
            nativeOnSystemInsetsChanged(0, 0, 0, 0);
        }
    }

    @SuppressWarnings("deprecation")
    @Override
    public void onBackPressed() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
            super.onBackPressed();
            return;
        }
        if (!nativeOnBackRequested()) {
            super.onBackPressed();
        }
    }

    private static native boolean nativeOnBackRequested();
    private static native void nativeOnSystemInsetsChanged(int left, int top, int right, int bottom);
    private static native void nativeOnFilePicked(
            String cachedPath,
            String displayName,
            String libraryUri,
            String error,
            boolean temporary);
    private static native void nativeOnFilePickedFd(
            int fd, String displayName, String libraryUri, String error);
    private static native void nativeOnImportBatchFinished(
            int importedCount, int failedCount, String errors);
    private static native void nativeOnCloudFileSelected(
            String uri, String displayName, long bytes);
    private static native void nativeOnCloudSelectionFinished(int failedCount, String errors);
    private static native void nativeOnCameraProfileFolderImportStarted(String displayName);
    private static native void nativeOnCameraProfileFolderPicked(
            String cachedPath, String displayName, int profileCount, String error);
    private static native void nativeOnExportPublished(String location, String error);

    /** Called from Rust's egui button. */
    public void openRawDocument() {
        runOnUiThread(() -> startActivityForResult(
                storageManager.createRawDocumentPickerIntent(), OPEN_RAW_DOCUMENT));
    }

    /** Selects RAWs for direct cloud upload without importing local-library copies. */
    public void openCloudRawDocuments() {
        runOnUiThread(() -> startActivityForResult(
                storageManager.createRawDocumentPickerIntent(), OPEN_CLOUD_RAW_DOCUMENT));
    }

    /** Opens Android's Storage Access Framework tree picker for DCP profile roots. */
    public void openCameraProfileFolder() {
        runOnUiThread(() -> startActivityForResult(
                profileImporter.createFolderPickerIntent(), OPEN_CAMERA_PROFILE_FOLDER));
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == OPEN_CAMERA_PROFILE_FOLDER) {
            profileImporter.handleFolderPickerResult(resultCode, data);
        } else if (requestCode == OPEN_CLOUD_RAW_DOCUMENT) {
            storageManager.handleCloudRawDocumentResult(resultCode, data);
        } else if (requestCode == OPEN_RAW_DOCUMENT) {
            storageManager.handleRawDocumentResult(resultCode, data);
        }
    }

    /** Mirrors the active Rust task into Android's notification shade. */
    public void updateBackgroundTaskNotification(
            String title,
            String phase,
            String detail,
            int progressPercent,
            int indeterminateFlag,
            int queuedCount) {
        TaskNotificationController controller = taskNotificationController;
        if (controller == null) {
            return;
        }
        controller.update(
                title,
                phase,
                detail,
                progressPercent,
                indeterminateFlag != 0,
                queuedCount);
    }

    /** Removes AuRaw's task-progress notification after the queue becomes idle. */
    public void clearBackgroundTaskNotification() {
        TaskNotificationController controller = taskNotificationController;
        if (controller != null) {
            controller.clear();
        }
    }

    /** Copies text through Android's native clipboard service. */
    public void copyTextToClipboard(String label, String text) {
        final String safeLabel = label == null || label.isEmpty()
                ? "AuRaw diagnostics"
                : label;
        final String safeText = text == null ? "" : text;
        runOnUiThread(() -> {
            ClipboardManager clipboard =
                    (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (clipboard == null) {
                Toast.makeText(this, "Clipboard is unavailable", Toast.LENGTH_SHORT).show();
                return;
            }
            clipboard.setPrimaryClip(ClipData.newPlainText(safeLabel, safeText));
            Toast.makeText(this, "Diagnostic log copied", Toast.LENGTH_SHORT).show();
        });
    }

    /** Device information displayed in AuRaw's in-app diagnostic log. */
    public String deviceDiagnostics() {
        return "manufacturer=" + Build.MANUFACTURER
                + "\nbrand=" + Build.BRAND
                + "\nmodel=" + Build.MODEL
                + "\ndevice=" + Build.DEVICE
                + "\nproduct=" + Build.PRODUCT
                + "\nandroid_release=" + Build.VERSION.RELEASE
                + "\nsdk=" + Build.VERSION.SDK_INT
                + "\nsupported_abis=" + String.join(",", Build.SUPPORTED_ABIS);
    }

    public String performanceSettingsPath() {
        return new File(getFilesDir(), "auraw-performance.json").getAbsolutePath();
    }

    public String gpuPipelineCacheDir() {
        return new File(getCacheDir(), "gpu-pipeline-cache").getAbsolutePath();
    }

    /** Materializes the bundled Lensfun XML files because its native API requires paths. */
    public String lensfunDatabaseDir() throws IOException {
        File destination = new File(getFilesDir(), "lensfun");
        copyAssetTree("lensfun", destination);
        return destination.getAbsolutePath();
    }

    private void copyAssetTree(String assetPath, File destination) throws IOException {
        String[] children = getAssets().list(assetPath);
        if (children != null && children.length > 0) {
            if (!destination.isDirectory() && !destination.mkdirs()) {
                throw new IOException("Could not create Lensfun directory " + destination);
            }
            for (String child : children) {
                copyAssetTree(assetPath + "/" + child, new File(destination, child));
            }
            return;
        }

        File parent = destination.getParentFile();
        if (parent == null || (!parent.isDirectory() && !parent.mkdirs())) {
            throw new IOException("Could not create Lensfun directory for " + destination);
        }
        try (InputStream input = getAssets().open(assetPath);
                FileOutputStream output = new FileOutputStream(destination, false)) {
            byte[] buffer = new byte[32 * 1024];
            int count;
            while ((count = input.read(buffer)) >= 0) {
                if (count > 0) {
                    output.write(buffer, 0, count);
                }
            }
            output.getFD().sync();
        }
    }

    @Override
    public void onRequestPermissionsResult(
            int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (taskNotificationController != null) {
            taskNotificationController.onRequestPermissionsResult(
                    requestCode, permissions, grantResults);
        }
        exportPublisher.onRequestPermissionsResult(requestCode, permissions, grantResults);
    }

    @Override
    protected void onDestroy() {
        if (taskNotificationController != null) {
            taskNotificationController.clear();
        }
        super.onDestroy();
    }
}
