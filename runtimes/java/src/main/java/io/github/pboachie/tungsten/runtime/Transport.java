// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.io.ByteArrayOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.net.ConnectException;
import java.net.URI;
import java.net.UnknownHostException;
import java.net.http.HttpClient;
import java.net.http.HttpConnectTimeoutException;
import java.net.http.HttpHeaders;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.net.http.HttpTimeoutException;
import java.nio.ByteBuffer;
import java.nio.channels.UnresolvedAddressException;
import java.time.Duration;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionStage;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.Flow;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import javax.net.ssl.SSLException;
import javax.net.ssl.SSLHandshakeException;

/**
 * One HTTP attempt over {@link HttpClient}: HTTP/1.1, no redirects (the runtime follows
 * same-origin redirects of reads itself), no cookie handler, no compression, and a deadline per
 * attempt that covers the response body too.
 */
final class Transport {
    private Transport() {}

    /** The default client. */
    static HttpClient newClient() {
        return HttpClient.newBuilder()
                .version(HttpClient.Version.HTTP_1_1)
                .followRedirects(HttpClient.Redirect.NEVER)
                .build();
    }

    enum Kind {
        RESPONSE,
        STREAMING,
        NOT_SENT,
        LOST,
        TIMEOUT,
        ABORTED
    }

    record Request(
            String url,
            String method,
            List<Serialize.Header> headers,
            Serialize.Payload body,
            Duration timeout,
            boolean stream,
            Duration idle) {}

    /** What one attempt gave. */
    static final class Outcome {
        Kind kind;
        int status;
        Map<String, String> headers = Map.of();
        byte[] body;
        boolean bodyOk;
        String bodyFailure;
        Body stream;
        String detail = "";
        boolean sent;

        static Outcome of(Kind kind, String detail) {
            Outcome o = new Outcome();
            o.kind = kind;
            o.detail = detail;
            return o;
        }
    }

    /** How a read of a response body ended. */
    enum Read {
        CHUNK,
        END,
        TIMEOUT,
        LOST
    }

    private static final Object END = new Object();

    /** A response body read chunk by chunk, each read waiting at most a given time. */
    static final class Body implements HttpResponse.BodySubscriber<Body> {
        private final LinkedBlockingQueue<Object> queue = new LinkedBlockingQueue<>();
        private final CompletableFuture<Body> self = CompletableFuture.completedFuture(this);
        private volatile Flow.Subscription subscription;
        private volatile boolean closed;
        private boolean finished;
        final Duration idle;
        byte[] chunk;

        Body(Duration idle) {
            this.idle = idle;
        }

        @Override
        public CompletionStage<Body> getBody() {
            return self;
        }

        @Override
        public void onSubscribe(Flow.Subscription s) {
            subscription = s;
            if (closed) {
                s.cancel();
            } else {
                s.request(1);
            }
        }

        @Override
        public void onNext(List<ByteBuffer> buffers) {
            int n = 0;
            for (ByteBuffer b : buffers) {
                n += b.remaining();
            }
            byte[] out = new byte[n];
            int at = 0;
            for (ByteBuffer b : buffers) {
                int len = b.remaining();
                b.get(out, at, len);
                at += len;
            }
            queue.add(out);
        }

        @Override
        public void onError(Throwable error) {
            queue.add(error);
        }

        @Override
        public void onComplete() {
            queue.add(END);
        }

        /** Reads the next chunk into {@link #chunk}, waiting at most wait. */
        Read read(Duration wait) {
            if (finished) {
                return Read.END;
            }
            Object item;
            try {
                long ms = Math.max(1, wait.toMillis());
                item = queue.poll(ms, TimeUnit.MILLISECONDS);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                close();
                return Read.LOST;
            }
            if (item == null) {
                return closed ? Read.LOST : Read.TIMEOUT;
            }
            if (item == END) {
                finished = true;
                return Read.END;
            }
            if (item instanceof Throwable) {
                finished = true;
                return Read.LOST;
            }
            chunk = (byte[]) item;
            Flow.Subscription s = subscription;
            if (s != null && !closed) {
                s.request(1);
            }
            return Read.CHUNK;
        }

        /** Reads at most the idle time of a stream. */
        Read read() {
            return read(idle);
        }

        /** Releases the connection; a blocked read ends as lost. */
        void close() {
            closed = true;
            Flow.Subscription s = subscription;
            if (s != null) {
                s.cancel();
            }
            queue.add(new IOException("closed"));
        }
    }

    static boolean isEventStream(Map<String, String> headers) {
        String v = headers.get("content-type");
        if (v == null) {
            return false;
        }
        return v.split(";", 2)[0].trim().equalsIgnoreCase("text/event-stream");
    }

    private static boolean has(Throwable t, Class<? extends Throwable> type) {
        for (Throwable c = t; c != null; c = c.getCause()) {
            if (type.isInstance(c)) {
                return true;
            }
        }
        return false;
    }

    private static String messages(Throwable t) {
        StringBuilder b = new StringBuilder();
        for (Throwable c = t; c != null; c = c.getCause()) {
            if (c.getMessage() != null) {
                b.append(c.getMessage().toLowerCase(Locale.ROOT)).append('\n');
            }
        }
        return b.toString();
    }

    /** The outcome of a failed exchange. */
    static Outcome failure(Throwable t) {
        if (has(t, HttpConnectTimeoutException.class)) {
            return Outcome.of(Kind.NOT_SENT, "ETIMEDOUT");
        }
        if (has(t, HttpTimeoutException.class)) {
            return Outcome.of(Kind.TIMEOUT, "");
        }
        String text = messages(t);
        if (has(t, UnresolvedAddressException.class) || has(t, UnknownHostException.class)) {
            return Outcome.of(Kind.NOT_SENT, "ENOTFOUND");
        }
        if (has(t, ConnectException.class)) {
            // HttpClient reports a refused connection as a ConnectException without a message.
            return Outcome.of(Kind.NOT_SENT, "ECONNREFUSED");
        }
        if (has(t, SSLHandshakeException.class)) {
            return Outcome.of(Kind.NOT_SENT, text.contains("certificat") ? "CERT_VERIFY_FAILED" : "TLS_ERROR");
        }
        if (has(t, SSLException.class)) {
            return Outcome.of(Kind.LOST, "TLS_ERROR");
        }
        if (text.contains("reset")) {
            return Outcome.of(Kind.LOST, "ECONNRESET");
        }
        if (has(t, EOFException.class) || text.contains("closed") || text.contains("no bytes")
                || text.contains("eof")) {
            return Outcome.of(Kind.LOST, "CONNECTION_CLOSED");
        }
        return Outcome.of(Kind.LOST, "network error");
    }

    private static Map<String, String> headersOf(HttpHeaders headers) {
        Map<String, String> out = new LinkedHashMap<>();
        for (Map.Entry<String, List<String>> e : headers.map().entrySet()) {
            String lower = e.getKey().toLowerCase(Locale.ROOT);
            if (lower.startsWith(":")) {
                continue;
            }
            for (String value : e.getValue()) {
                String known = out.get(lower);
                out.put(lower, known != null && !lower.equals("set-cookie") ? known + ", " + value : value);
            }
        }
        return out;
    }

    private static HttpRequest build(Request req) {
        HttpRequest.Builder b = HttpRequest.newBuilder(URI.create(req.url()));
        String method = req.method();
        String contentType = null;
        HttpRequest.BodyPublisher publisher = null;
        Serialize.Payload body = req.body();
        if (body != null && !body.empty) {
            if (body.parts != null) {
                Map.Entry<byte[], String> mp = Serialize.multipart(body.parts);
                publisher = HttpRequest.BodyPublishers.ofByteArray(mp.getKey());
                contentType = mp.getValue();
            } else {
                publisher = HttpRequest.BodyPublishers.ofByteArray(body.bytes);
            }
        }
        if (publisher != null) {
            b.method(method, publisher);
        } else if (method.equals("GET")) {
            b.GET();
        } else if (method.equals("DELETE")) {
            b.DELETE();
        } else {
            // A POST, PUT or PATCH without a body says so (content-length: 0, RFC 9110 8.6), as
            // fetch and httpx do.
            b.method(method, HttpRequest.BodyPublishers.noBody());
        }
        boolean accept = false;
        for (Serialize.Header h : req.headers()) {
            if (contentType != null && h.name().equalsIgnoreCase("content-type")) {
                continue;
            }
            b.header(h.name(), h.value());
            accept |= h.name().equalsIgnoreCase("accept");
        }
        if (contentType != null) {
            b.header("Content-Type", contentType);
        }
        // Without a declared media type the request accepts anything, as the HTTP clients of the
        // other runtimes send it.
        if (!accept) {
            b.header("Accept", "*/*");
        }
        return b.build();
    }

    /** Sends once. It never throws: every outcome is an {@link Outcome}. */
    static Outcome attempt(HttpClient http, Request req) {
        HttpRequest request;
        try {
            request = build(req);
        } catch (IllegalArgumentException e) {
            return Outcome.of(Kind.NOT_SENT, "INVALID_REQUEST");
        }
        long deadline = System.nanoTime() + req.timeout().toNanos();
        Duration idle = req.idle() != null ? req.idle() : req.timeout();
        Body subscriber = new Body(idle);
        CompletableFuture<HttpResponse<Body>> future;
        try {
            future = http.sendAsync(request, info -> subscriber);
        } catch (IllegalArgumentException e) {
            return Outcome.of(Kind.NOT_SENT, "INVALID_REQUEST");
        }
        HttpResponse<Body> response;
        try {
            response = future.get(Math.max(1, req.timeout().toMillis()), TimeUnit.MILLISECONDS);
        } catch (TimeoutException e) {
            future.cancel(true);
            subscriber.close();
            return Outcome.of(Kind.TIMEOUT, "");
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            future.cancel(true);
            subscriber.close();
            Outcome o = Outcome.of(Kind.ABORTED, "");
            o.sent = true;
            return o;
        } catch (ExecutionException e) {
            return failure(e.getCause() != null ? e.getCause() : e);
        }
        Outcome out = new Outcome();
        out.status = response.statusCode();
        out.headers = headersOf(response.headers());
        if (req.stream() && out.status >= 200 && out.status <= 299 && isEventStream(out.headers)) {
            out.kind = Kind.STREAMING;
            out.stream = subscriber;
            return out;
        }
        out.kind = Kind.RESPONSE;
        ByteArrayOutputStream data = new ByteArrayOutputStream();
        while (true) {
            long left = deadline - System.nanoTime();
            if (left <= 0) {
                subscriber.close();
                out.bodyFailure = "timeout";
                return out;
            }
            Read r = subscriber.read(Duration.ofNanos(left));
            if (r == Read.CHUNK) {
                data.writeBytes(subscriber.chunk);
            } else if (r == Read.END) {
                out.body = data.toByteArray();
                out.bodyOk = true;
                return out;
            } else if (r == Read.TIMEOUT) {
                subscriber.close();
                out.bodyFailure = "timeout";
                return out;
            } else {
                out.bodyFailure = "broken";
                return out;
            }
        }
    }
}
