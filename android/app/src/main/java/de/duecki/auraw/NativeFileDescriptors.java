package de.duecki.auraw;

import android.os.ParcelFileDescriptor;

/** Explicit ownership handoff helpers for descriptors transferred to Rust. */
final class NativeFileDescriptors {
    private NativeFileDescriptors() {}

    /**
     * Detaches a descriptor and transfers sole close responsibility to native code.
     *
     * <p>The parcel descriptor is always closed on this side. After a successful
     * detach that close does not close the raw descriptor; it only releases the
     * Java wrapper. This makes the ownership boundary explicit and leak-safe if
     * an exception occurs before the handoff completes.
     */
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

    /** Closes a descriptor when a Java-to-Rust callback could not accept ownership. */
    static void closeTransferred(int fd) {
        if (fd < 0) {
            return;
        }
        try {
            ParcelFileDescriptor.adoptFd(fd).close();
        } catch (Exception ignored) {
            // Ownership cleanup is best effort after a failed callback.
        }
    }
}
