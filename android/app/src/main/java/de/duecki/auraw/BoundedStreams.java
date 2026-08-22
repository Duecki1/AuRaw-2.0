package de.duecki.auraw;

import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;

/** Bounded stream copying that is safe for Android content providers. */
final class BoundedStreams {
    private static final int BUFFER_BYTES = 256 * 1024;

    private BoundedStreams() {}

    /**
     * Copies at most {@code maximumBytes} and returns the copied byte count.
     *
     * <p>A zero-byte bulk read is followed by a one-byte read so a broken or
     * slow content provider cannot cause an infinite no-progress loop.
     */
    static long copy(
            InputStream input,
            OutputStream output,
            long maximumBytes,
            String limitExceededMessage) throws IOException {
        if (maximumBytes < 0L) {
            throw new IllegalArgumentException("maximumBytes must not be negative");
        }
        byte[] buffer = new byte[BUFFER_BYTES];
        long copied = 0L;
        while (true) {
            int count = input.read(buffer);
            if (count < 0) {
                return copied;
            }
            if (count == 0) {
                int value = input.read();
                if (value < 0) {
                    return copied;
                }
                copied = checkedLength(copied, 1, maximumBytes, limitExceededMessage);
                output.write(value);
                continue;
            }
            copied = checkedLength(copied, count, maximumBytes, limitExceededMessage);
            output.write(buffer, 0, count);
        }
    }

    private static long checkedLength(
            long copied,
            int count,
            long maximumBytes,
            String limitExceededMessage) throws StorageLimitExceededException {
        if (count < 0 || copied > maximumBytes - count) {
            throw new StorageLimitExceededException(limitExceededMessage);
        }
        return copied + count;
    }
}

/** A domain-specific failure for an input that violates an explicit storage bound. */
final class StorageLimitExceededException extends IllegalStateException {
    StorageLimitExceededException(String message) {
        super(message);
    }
}
