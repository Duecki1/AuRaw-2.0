package de.duecki.auraw;

import android.content.ContentResolver;
import android.net.Uri;

final class PickerLocationStore {
    private static final String PREFERENCES = "auraw-picker-locations";

    private final AndroidStorageAccess storage;

    PickerLocationStore(AndroidStorageAccess storage) {
        this.storage = storage;
    }

    Uri readContentUri(String key) {
        String uriText = storage
                .getSharedPreferences(PREFERENCES, AuRawActivity.MODE_PRIVATE)
                .getString(key, "");
        if (uriText == null || uriText.isEmpty()) {
            return null;
        }
        try {
            Uri uri = Uri.parse(uriText);
            return ContentResolver.SCHEME_CONTENT.equals(uri.getScheme()) ? uri : null;
        } catch (RuntimeException ignored) {
            return null;
        }
    }

    void writeContentUri(String key, Uri uri) {
        if (uri == null || !ContentResolver.SCHEME_CONTENT.equals(uri.getScheme())) {
            return;
        }
        storage.getSharedPreferences(PREFERENCES, AuRawActivity.MODE_PRIVATE)
                .edit()
                .putString(key, uri.toString())
                .apply();
    }

    void clear(String key) {
        storage.getSharedPreferences(PREFERENCES, AuRawActivity.MODE_PRIVATE)
                .edit()
                .remove(key)
                .apply();
    }
}
