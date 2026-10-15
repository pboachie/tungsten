// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.io.BufferedInputStream;
import java.io.ByteArrayOutputStream;
import java.io.EOFException;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.ConnectException;
import java.net.InetSocketAddress;
import java.net.Socket;
import java.net.SocketTimeoutException;
import java.net.URI;
import java.net.UnknownHostException;
import java.nio.charset.StandardCharsets;
import java.time.Duration;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.LinkedBlockingQueue;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import javax.net.ssl.SSLException;
import javax.net.ssl.SSLHandshakeException;
import javax.net.ssl.SSLParameters;
import javax.net.ssl.SSLSocket;
import javax.net.ssl.SSLSocketFactory;

/**
 * One HTTP/1.1 attempt over a socket of its own: no connection reuse, no redirects (the runtime
 * follows same-origin redirects of reads itself), no cookies, no compression, and a deadline per
 * attempt that covers the response body too.
 *
 * <p>Not {@code java.net.http.HttpClient}: that client sends a GET or HEAD a second time when the
 * connection fails before the response starts, an attempt the runtime could neither count nor
 * refuse (its retry policy, {@code OUTCOME_UNKNOWN} and the contract's call counts depend on
 * every attempt being its own).
 */
final class Transport {
    private Transport() {}

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
            Duration idle,
            Long maxResponseBytes) {
        Request(String url, String method, List<Serialize.Header> headers, Serialize.Payload body,
                Duration timeout, boolean stream, Duration idle) {
            this(url, method, headers, body, timeout, stream, idle, null);
        }
    }

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
    private static final int MAX_LINE = 65536;
    private static final int MAX_HEAD = 262144;

    /** Headers the transport writes itself; a request that sets one is refused. */
    private static final Set<String> RESTRICTED =
            Set.of("connection", "content-length", "expect", "host", "upgrade");

    /**
     * A response body read chunk by chunk, each read waiting at most a given time. A thread of
     * the attempt reads the socket and queues the chunks.
     */
    static final class Body {
        // Apply backpressure when the caller consumes a stream slowly.
        private final LinkedBlockingQueue<Object> queue = new LinkedBlockingQueue<>(64);
        private volatile boolean closed;
        private volatile Socket socket;
        private boolean finished;
        final Duration idle;
        byte[] chunk;

        Body(Duration idle) {
            this.idle = idle;
        }

        void attach(Socket s) throws IOException {
            socket = s;
            if (closed) {
                s.close();
                throw new IOException("closed");
            }
        }

        void push(Object item) {
            try {
                while (!closed && !queue.offer(item, 100, TimeUnit.MILLISECONDS)) {
                    // A timed offer lets close release the producer as well as the reader.
                }
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
                close();
            }
        }

        /** Reads the next chunk into {@link #chunk}, waiting at most wait. */
        Read read(Duration wait) {
            if (closed) {
                return Read.LOST;
            }
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
            return Read.CHUNK;
        }

        /** Reads at most the idle time of a stream. */
        Read read() {
            return read(idle);
        }

        /** Releases the connection; a blocked read ends as lost. */
        void close() {
            closed = true;
            Socket s = socket;
            if (s != null) {
                try {
                    s.close();
                } catch (IOException ignored) {
                    // closing is best effort
                }
            }
            queue.clear();
            queue.offer(new IOException("closed"));
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

    /** A failure while connecting: nothing was sent. */
    private static final class NotConnected extends IOException {
        private static final long serialVersionUID = 1L;

        NotConnected(IOException cause) {
            super(cause);
        }
    }

    /** The outcome of a failed exchange. */
    static Outcome failure(Throwable t) {
        String text = messages(t);
        if (has(t, NotConnected.class)) {
            if (has(t, SocketTimeoutException.class)) {
                return Outcome.of(Kind.NOT_SENT, "ETIMEDOUT");
            }
            if (has(t, UnknownHostException.class)) {
                return Outcome.of(Kind.NOT_SENT, "ENOTFOUND");
            }
            if (has(t, SSLHandshakeException.class)) {
                return Outcome.of(Kind.NOT_SENT, text.contains("certificat") ? "CERT_VERIFY_FAILED" : "TLS_ERROR");
            }
            if (has(t, SSLException.class)) {
                return Outcome.of(Kind.NOT_SENT, "TLS_ERROR");
            }
            if (has(t, ConnectException.class) && !text.contains("refused")) {
                return Outcome.of(Kind.NOT_SENT, "network error");
            }
            return Outcome.of(Kind.NOT_SENT, "ECONNREFUSED");
        }
        if (has(t, SSLException.class)) {
            return Outcome.of(Kind.LOST, "TLS_ERROR");
        }
        if (text.contains("reset") || text.contains("broken pipe")) {
            return Outcome.of(Kind.LOST, "ECONNRESET");
        }
        if (has(t, EOFException.class) || text.contains("closed")) {
            return Outcome.of(Kind.LOST, "CONNECTION_CLOSED");
        }
        return Outcome.of(Kind.LOST, "network error");
    }

    /** The parts of a request URL, or null when it cannot be sent. */
    private record Target(boolean tls, String host, int port, String hostHeader, String path) {}

    private static Target target(String url) {
        URI uri;
        try {
            uri = URI.create(url);
        } catch (IllegalArgumentException e) {
            return null;
        }
        String scheme = uri.getScheme() == null ? "" : uri.getScheme().toLowerCase(Locale.ROOT);
        boolean tls;
        if (scheme.equals("https")) {
            tls = true;
        } else if (scheme.equals("http")) {
            tls = false;
        } else {
            return null;
        }
        String host = uri.getHost();
        if (host == null || host.isEmpty()) {
            return null;
        }
        int port = uri.getPort();
        int defaultPort = tls ? 443 : 80;
        String hostHeader = port == -1 || port == defaultPort ? host : host + ":" + port;
        String bare = host.startsWith("[") && host.endsWith("]") ? host.substring(1, host.length() - 1) : host;
        String path = uri.getRawPath() == null || uri.getRawPath().isEmpty() ? "/" : uri.getRawPath();
        if (uri.getRawQuery() != null) {
            path = path + "?" + uri.getRawQuery();
        }
        return new Target(tls, bare, port == -1 ? defaultPort : port, hostHeader, path);
    }

    private static boolean isToken(String s) {
        if (s.isEmpty()) {
            return false;
        }
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c > 126 || c <= 32 || "\"(),/:;<=>?@[\\]{}".indexOf(c) >= 0) {
                return false;
            }
        }
        return true;
    }

    private static boolean isFieldValue(String s) {
        for (int i = 0; i < s.length(); i++) {
            char c = s.charAt(i);
            if (c == '\r' || c == '\n' || c == 0) {
                return false;
            }
        }
        return true;
    }

    /** The request head and body, or null when the request cannot be sent. */
    private static byte[][] encode(Request req, Target t) {
        String method = req.method();
        if (!isToken(method)) {
            return null;
        }
        byte[] body = null;
        String contentType = null;
        Serialize.Payload payload = req.body();
        if (payload != null && !payload.empty) {
            if (payload.parts != null) {
                Map.Entry<byte[], String> mp = Serialize.multipart(payload.parts);
                body = mp.getKey();
                contentType = mp.getValue();
            } else {
                body = payload.bytes;
            }
        }
        StringBuilder head = new StringBuilder();
        head.append(method).append(' ').append(t.path()).append(" HTTP/1.1\r\n");
        head.append("Host: ").append(t.hostHeader()).append("\r\n");
        boolean accept = false;
        for (Serialize.Header h : req.headers()) {
            if (contentType != null && h.name().equalsIgnoreCase("content-type")) {
                continue;
            }
            if (!isToken(h.name()) || !isFieldValue(h.value())
                    || RESTRICTED.contains(h.name().toLowerCase(Locale.ROOT))) {
                return null;
            }
            head.append(h.name()).append(": ").append(h.value()).append("\r\n");
            accept |= h.name().equalsIgnoreCase("accept");
        }
        if (contentType != null) {
            head.append("Content-Type: ").append(contentType).append("\r\n");
        }
        // Without a declared media type the request accepts anything, as the HTTP clients of the
        // other runtimes send it.
        if (!accept) {
            head.append("Accept: */*\r\n");
        }
        if (body != null) {
            head.append("Content-Length: ").append(body.length).append("\r\n");
        } else if (!method.equals("GET") && !method.equals("DELETE") && !method.equals("HEAD")) {
            // A POST, PUT or PATCH without a body says so (content-length: 0, RFC 9110 8.6), as
            // fetch and httpx do.
            head.append("Content-Length: 0\r\n");
        }
        head.append("\r\n");
        return new byte[][] {head.toString().getBytes(StandardCharsets.UTF_8), body};
    }

    /** The status and headers of a response. */
    private record Head(int status, Map<String, String> headers) {}

    private static String line(InputStream in, boolean first) throws IOException {
        ByteArrayOutputStream b = new ByteArrayOutputStream();
        while (true) {
            int c = in.read();
            if (c < 0) {
                if (first && b.size() == 0) {
                    throw new EOFException("the connection closed before a response");
                }
                throw new EOFException("the connection closed in the response head");
            }
            if (c == '\n') {
                break;
            }
            b.write(c);
            if (b.size() > MAX_LINE) {
                throw new IOException("response line too long");
            }
        }
        byte[] bytes = b.toByteArray();
        int n = bytes.length;
        if (n > 0 && bytes[n - 1] == '\r') {
            n--;
        }
        return new String(bytes, 0, n, StandardCharsets.ISO_8859_1);
    }

    private static Head readHead(InputStream in) throws IOException {
        while (true) {
            String status = line(in, true);
            if (!status.startsWith("HTTP/1.") || status.length() < 12 || status.charAt(8) != ' ') {
                throw new IOException("malformed response status line");
            }
            int code;
            try {
                code = Integer.parseInt(status.substring(9, 12));
            } catch (NumberFormatException e) {
                throw new IOException("malformed response status line");
            }
            Map<String, String> headers = new LinkedHashMap<>();
            String last = null;
            int size = 0;
            while (true) {
                String l = line(in, false);
                if (l.isEmpty()) {
                    break;
                }
                size += l.length();
                if (size > MAX_HEAD) {
                    throw new IOException("response head too large");
                }
                if ((l.charAt(0) == ' ' || l.charAt(0) == '\t') && last != null) {
                    headers.put(last, headers.get(last) + " " + l.trim());
                    continue;
                }
                int colon = l.indexOf(':');
                if (colon <= 0) {
                    throw new IOException("malformed response header");
                }
                String name = l.substring(0, colon).trim().toLowerCase(Locale.ROOT);
                String value = l.substring(colon + 1).trim();
                String known = headers.get(name);
                headers.put(name, known != null && !name.equals("set-cookie") ? known + ", " + value : value);
                last = name;
            }
            if (code >= 100 && code < 200 && code != 101) {
                continue;
            }
            return new Head(code, headers);
        }
    }

    private static void copy(InputStream in, Body body, long limit) throws IOException {
        byte[] buf = new byte[8192];
        long left = limit;
        while (left != 0) {
            int want = left < 0 ? buf.length : (int) Math.min(buf.length, left);
            int n = in.read(buf, 0, want);
            if (n < 0) {
                if (left > 0) {
                    throw new EOFException("the connection closed in the response body");
                }
                return;
            }
            if (n > 0) {
                byte[] out = new byte[n];
                System.arraycopy(buf, 0, out, 0, n);
                body.push(out);
                if (left > 0) {
                    left -= n;
                }
            }
        }
    }

    private static void chunked(InputStream in, Body body) throws IOException {
        while (true) {
            String size = line(in, false);
            int semi = size.indexOf(';');
            if (semi >= 0) {
                size = size.substring(0, semi);
            }
            long n;
            try {
                n = Long.parseLong(size.trim(), 16);
            } catch (NumberFormatException e) {
                throw new IOException("malformed chunk size");
            }
            if (n < 0) {
                throw new IOException("malformed chunk size");
            }
            if (n == 0) {
                while (!line(in, false).isEmpty()) {
                    // trailers are ignored
                }
                return;
            }
            copy(in, body, n);
            if (!line(in, false).isEmpty()) {
                throw new IOException("malformed chunk");
            }
        }
    }

    private static void pump(InputStream in, Body body, String method, Head head) throws IOException {
        int s = head.status();
        if (method.equals("HEAD") || s == 204 || s == 304 || (s >= 100 && s < 200)) {
            return;
        }
        String te = head.headers().get("transfer-encoding");
        if (te != null && te.toLowerCase(Locale.ROOT).contains("chunked")) {
            chunked(in, body);
            return;
        }
        String cl = head.headers().get("content-length");
        if (cl != null) {
            long n;
            try {
                n = Long.parseLong(cl.split(",", 2)[0].trim());
            } catch (NumberFormatException e) {
                throw new IOException("malformed content-length");
            }
            if (n < 0) {
                throw new IOException("malformed content-length");
            }
            copy(in, body, n);
            return;
        }
        copy(in, body, -1);
    }

    private static Socket connect(Target t, Duration timeout, SSLSocketFactory tls, Body body) throws IOException {
        Socket raw = new Socket();
        body.attach(raw);
        try {
            InetSocketAddress address = new InetSocketAddress(t.host(), t.port());
            if (address.isUnresolved()) {
                throw new UnknownHostException(t.host());
            }
            int ms = (int) Math.max(1, Math.min(Integer.MAX_VALUE, timeout.toMillis()));
            raw.connect(address, ms);
            raw.setTcpNoDelay(true);
            if (!t.tls()) {
                return raw;
            }
            raw.setSoTimeout(ms);
            SSLSocket ssl = (SSLSocket) tls.createSocket(raw, t.host(), t.port(), true);
            body.attach(ssl);
            SSLParameters params = ssl.getSSLParameters();
            params.setEndpointIdentificationAlgorithm("HTTPS");
            ssl.setSSLParameters(params);
            ssl.startHandshake();
            ssl.setSoTimeout(0);
            return ssl;
        } catch (IOException e) {
            try {
                raw.close();
            } catch (IOException ignored) {
                // closing is best effort
            }
            throw new NotConnected(e);
        }
    }

    /** Sends once. It never throws: every outcome is an {@link Outcome}. */
    static Outcome attempt(SSLSocketFactory tls, Request req) {
        Target t = target(req.url());
        if (t == null) {
            return Outcome.of(Kind.NOT_SENT, "INVALID_REQUEST");
        }
        byte[][] wire;
        try {
            wire = encode(req, t);
        } catch (IllegalArgumentException e) {
            wire = null;
        }
        if (wire == null) {
            return Outcome.of(Kind.NOT_SENT, "INVALID_REQUEST");
        }
        long deadline = System.nanoTime() + req.timeout().toNanos();
        Duration idle = req.idle() != null ? req.idle() : req.timeout();
        Body body = new Body(idle);
        CompletableFuture<Head> head = new CompletableFuture<>();
        byte[][] request = wire;
        Thread worker = new Thread(() -> {
            Socket socket = null;
            try {
                socket = connect(t, req.timeout(), tls, body);
                OutputStream out = socket.getOutputStream();
                out.write(request[0]);
                if (request[1] != null) {
                    out.write(request[1]);
                }
                out.flush();
                InputStream in = new BufferedInputStream(socket.getInputStream());
                Head h = readHead(in);
                head.complete(h);
                pump(in, body, req.method(), h);
                body.push(END);
            } catch (IOException | RuntimeException e) {
                if (!head.completeExceptionally(e)) {
                    body.push(e);
                }
            } finally {
                if (socket != null) {
                    try {
                        socket.close();
                    } catch (IOException ignored) {
                        // closing is best effort
                    }
                }
            }
        }, "tungsten-http");
        worker.setDaemon(true);
        worker.start();
        Head response;
        try {
            response = head.get(Math.max(1, req.timeout().toMillis()), TimeUnit.MILLISECONDS);
        } catch (TimeoutException e) {
            body.close();
            return Outcome.of(Kind.TIMEOUT, "");
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            body.close();
            Outcome o = Outcome.of(Kind.ABORTED, "");
            o.sent = true;
            return o;
        } catch (ExecutionException e) {
            return failure(e.getCause() != null ? e.getCause() : e);
        }
        Outcome out = new Outcome();
        out.status = response.status();
        out.headers = response.headers();
        if (req.stream() && out.status >= 200 && out.status <= 299 && isEventStream(out.headers)) {
            out.kind = Kind.STREAMING;
            out.stream = body;
            return out;
        }
        out.kind = Kind.RESPONSE;
        Long cap = req.maxResponseBytes();
        String length = out.headers.get("content-length");
        if (cap != null && length != null && !req.method().equals("HEAD") && out.status != 204 && out.status != 304) {
            try {
                if (Long.parseLong(length.trim()) > cap) {
                    body.close();
                    out.bodyFailure = "oversized";
                    return out;
                }
            } catch (NumberFormatException ignored) {
                // The counting read remains authoritative for invalid/missing lengths.
            }
        }
        ByteArrayOutputStream data = new ByteArrayOutputStream();
        while (true) {
            long left = deadline - System.nanoTime();
            if (left <= 0) {
                body.close();
                out.bodyFailure = "timeout";
                return out;
            }
            Read r = body.read(Duration.ofNanos(left));
            if (r == Read.CHUNK) {
                if (cap != null && (long) data.size() + body.chunk.length > cap) {
                    body.close();
                    out.bodyFailure = "oversized";
                    return out;
                }
                data.writeBytes(body.chunk);
            } else if (r == Read.END) {
                out.body = data.toByteArray();
                out.bodyOk = true;
                return out;
            } else if (r == Read.TIMEOUT) {
                body.close();
                out.bodyFailure = "timeout";
                return out;
            } else {
                out.bodyFailure = "broken";
                return out;
            }
        }
    }
}
