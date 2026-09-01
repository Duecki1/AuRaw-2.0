package de.duecki.calibraw;

import android.os.ParcelFileDescriptor;

final class NativeFileDescriptors {
    private NativeFileDescriptors() {}

    // Success transfers sole close ownership to Rust; Java always releases its wrapper.
    static int detach(ParcelFileDescriptor descriptor, String missingDescriptorMessage)
            throws Exception {
        if (descriptor == null) {
            throw new IllegalStateException(missingDescriptorMessage);
        }
        try {
            int fd = descriptor.detachFd();
            if (fd < 0) {
                throw new IllegalStateException("Android returned an invalid file descriptor");
            }
            return fd;
        } finally {
            descriptor.close();
        }
    }

    static void closeTransferred(int fd) {
        if (fd < 0) {
            return;
        }
        try {
            ParcelFileDescriptor.adoptFd(fd).close();
        } catch (Exception ignored) {
        }
    }
}
