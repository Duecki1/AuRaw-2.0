package de.duecki.auraw;

import android.app.NativeActivity;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.os.Bundle;
import android.provider.OpenableColumns;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.util.Locale;

public final class AuRawActivity extends NativeActivity {
    private static final int OPEN_RAW_DOCUMENT = 1001;

    static {
        System.loadLibrary("auraw");
    }

    private static native void nativeOnFilePicked(
            String cachedPath,
            String displayName,
            String error);

    /** Called from Rust's egui button. */
    public void openRawDocument() {
        runOnUiThread(this::launchRawDocumentPicker);
    }

    private void launchRawDocumentPicker() {
        Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
        intent.addCategory(Intent.CATEGORY_OPENABLE);
        // RAW MIME types are inconsistent between camera vendors and document
        // providers, so extension filtering here would hide valid files.
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
            nativeOnFilePicked("", "", "");
            return;
        }

        Uri uri = data.getData();
        String displayName = queryDisplayName(uri);
        new Thread(
                () -> importDocument(uri, displayName),
                "AuRaw document import").start();
    }

    private void importDocument(Uri uri, String displayName) {
        try {
            String suffix = suffixFor(displayName);
            File imported = File.createTempFile("auraw-import-", suffix, getCacheDir());
            try (InputStream input = getContentResolver().openInputStream(uri);
                 FileOutputStream output = new FileOutputStream(imported)) {
                if (input == null) {
                    throw new IllegalStateException("The document provider returned no input stream");
                }
                byte[] buffer = new byte[1024 * 1024];
                int count;
                while ((count = input.read(buffer)) >= 0) {
                    output.write(buffer, 0, count);
                }
            }
            nativeOnFilePicked(imported.getAbsolutePath(), displayName, "");
        } catch (Exception error) {
            nativeOnFilePicked("", displayName, error.toString());
        }
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
