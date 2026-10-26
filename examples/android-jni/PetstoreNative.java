// SPDX-License-Identifier: AGPL-3.0-only
package example.petstore;

import java.util.concurrent.locks.ReentrantReadWriteLock;

/**
 * A Petstore client backed by the native library {@code petstore_jni}. Calls block the calling
 * thread, so call them from a background thread or coroutine dispatcher, never the Android main
 * thread. Calls may run concurrently; {@link #close()} cancels calls in flight, waits for them to
 * return and frees the native handle, after which every call throws {@link IllegalStateException}.
 */
public final class PetstoreNative implements AutoCloseable {
    static {
        System.loadLibrary("petstore_jni");
    }

    private final ReentrantReadWriteLock lock = new ReentrantReadWriteLock();
    private long handle;

    /**
     * @param baseUrl the API's base URL
     * @param deadlineMs the total deadline of one call (authentication, retries and the body)
     * @param maxResponseBytes the largest response body accepted
     */
    public PetstoreNative(String baseUrl, long deadlineMs, long maxResponseBytes) {
        handle = create(baseUrl, deadlineMs, maxResponseBytes);
    }

    /** The pets as a JSON array; throws {@link PetstoreException} with the diagnostic envelope. */
    public String listPets() {
        lock.readLock().lock();
        try {
            return listPets(live());
        } finally {
            lock.readLock().unlock();
        }
    }

    @Override
    public void close() {
        // Cancel first under the read lock, so calls in flight return promptly...
        lock.readLock().lock();
        try {
            if (handle != 0) {
                cancel(handle);
            }
        } finally {
            lock.readLock().unlock();
        }
        // ...then free the handle once no call holds it.
        lock.writeLock().lock();
        try {
            if (handle != 0) {
                destroy(handle);
                handle = 0;
            }
        } finally {
            lock.writeLock().unlock();
        }
    }

    private long live() {
        if (handle == 0) {
            throw new IllegalStateException("the client is closed");
        }
        return handle;
    }

    private static native long create(String baseUrl, long deadlineMs, long maxResponseBytes);

    private static native String listPets(long handle);

    private static native void cancel(long handle);

    private static native void destroy(long handle);
}
