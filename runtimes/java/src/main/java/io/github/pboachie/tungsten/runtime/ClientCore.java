// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.net.URI;
import java.net.URISyntaxException;
import java.time.Duration;
import java.util.ArrayList;
import java.util.Collection;
import java.util.HashMap;
import java.util.HashSet;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;
import java.util.Set;
import java.util.TreeMap;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.function.Consumer;
import javax.net.ssl.SSLSocketFactory;

/**
 * The engine behind every generated client: pre-flight validation, auth, idempotency keys,
 * confirmation rules, sending with tier-aware retries and classification into a result or the
 * error envelope. It is safe for concurrent use. API and transport failures are {@link
 * Result.Err} values carrying the envelope; nothing here throws for them.
 */
public final class ClientCore {
    /** The number of same-origin redirects one read follows. */
    static final int MAX_REDIRECTS = 5;

    static final int MAX_ARG_DEPTH = 256;

    final ApiDescriptor api;
    final Map<String, AuthCredential> auth;
    final Map<String, String> headers;
    final String baseUrl;
    final Duration timeout;
    final int maxEventBytes;
    final int maxCollectBytes;
    final Duration maxCollectTime;
    final int maxReconnects;
    final Duration reconnectMax;
    final Duration idleTimeout;
    final RetryOptions retries;
    final IdempotencyStore store;
    final List<Middleware> middleware;
    final ClientOptions.ValidateResponses validateResponses;
    final Consumer<Diagnostic> onDiagnostic;
    final byte[] confirmationKey;
    final SSLSocketFactory http;
    final ClientOptions options;
    private final boolean redirectsDisabled;

    /** Generated clients call this when their manifest forbids redirects. */
    public static void requireRedirectPolicyV1() {}
    final TokenStore tokenStore;
    final RuntimeIdentity identity;
    final Auth authResolver;

    private final Map<String, OperationDescriptor> registry = new HashMap<>();
    private final Map<String, UsedToken> usedTokens = new HashMap<>();
    private final Object keyLock = new Object();

    private record UsedToken(String bind, boolean hasBind, long expiry) {}

    /**
     * A client of an API.
     *
     * @param api the API descriptor
     * @param operations the operations the client resolves by id (verification hooks, endpoint
     *     previews, macro steps); operations passed to a call are registered too
     * @param options the options
     * @throws IllegalArgumentException when the base URL is not a valid absolute http(s) URL
     *     without credentials
     */
    public ClientCore(ApiDescriptor api, Collection<OperationDescriptor> operations, ClientOptions options) {
        this.api = api;
        redirectsDisabled = api.redirects() == RedirectPolicy.NONE || options.redirects == RedirectPolicy.NONE;
        this.options = options;
        this.auth = options.auth;
        this.headers = options.headers;
        String base = options.baseUrl;
        if ((base == null || base.isEmpty()) && !api.servers().isEmpty()) {
            base = api.servers().get(0);
        }
        if (base != null && !base.isEmpty() && !validUrl(base)) {
            throw new IllegalArgumentException("the base URL is not a valid absolute http(s) URL without credentials");
        }
        this.baseUrl = base == null ? "" : base;
        this.timeout = options.timeout;
        this.maxEventBytes = options.maxEventBytes;
        this.maxCollectBytes = options.maxCollectBytes;
        this.maxCollectTime = options.maxCollectTime;
        this.maxReconnects = options.maxReconnects;
        this.reconnectMax = options.reconnectMax;
        this.idleTimeout = options.idleTimeout;
        this.retries = options.retries;
        this.store = options.idempotencyStore;
        this.middleware = options.middleware;
        this.validateResponses = options.validateResponses;
        this.onDiagnostic = options.onDiagnostic;
        this.confirmationKey = options.confirmationKey;
        this.http = options.sslSocketFactory != null
                ? options.sslSocketFactory
                : (SSLSocketFactory) SSLSocketFactory.getDefault();
        this.tokenStore = options.tokenStore;
        this.identity = options.identity;
        this.authResolver = new Auth(this);
        for (OperationDescriptor op : operations) {
            register(op);
        }
    }

    /** A client of every operation of a descriptor document. */
    public ClientCore(Descriptors descriptors, ClientOptions options) {
        this(descriptors.api(), descriptors.operations().values(), options);
    }

    /** The API descriptor. */
    public ApiDescriptor api() {
        return api;
    }

    /** The identity the runtime reports. */
    public RuntimeIdentity identity() {
        return identity;
    }

    /** Makes an operation resolvable by id (the first registration of an id stays). */
    public void register(OperationDescriptor op) {
        if (op == null || op.id() == null || op.id().isEmpty()) {
            return;
        }
        synchronized (registry) {
            registry.putIfAbsent(op.id(), op);
        }
    }

    /** A registered operation, or null. */
    public OperationDescriptor operation(String id) {
        synchronized (registry) {
            return registry.get(id);
        }
    }

    // ------------------------------------------------------------ public API

    /**
     * Validates, authenticates, applies idempotency and confirmation rules, sends with retries and
     * classifies the response. The value is the decoded wire body.
     *
     * @param args the arguments object (keyed by arguments key); null is no arguments
     */
    public Result<Object> call(OperationDescriptor op, Object args, CallOptions opts) {
        try {
            return callWith(op, args, opts, null, null);
        } catch (TungstenException e) {
            return e.toResult();
        }
    }

    /**
     * Renders the request without sending it (or runs the operation's header or endpoint
     * preview). Mutating operations get a confirmation token bound to these exact arguments for
     * five minutes.
     */
    public Result<PreviewResult> preview(OperationDescriptor op, Object args, CallOptions opts) {
        try {
            return Previews.preview(this, op, args, opts);
        } catch (TungstenException e) {
            return e.toResult();
        }
    }

    /** The pages of a paginated operation; the first error ends the iteration. */
    public Pages pages(OperationDescriptor op, Object args, CallOptions opts) {
        return new Pages(this, op, args, opts);
    }

    /**
     * Calls a read operation until the predicate holds or the budget runs out; on a timeout the
     * last body is returned with {@link Polled#timedOut} true.
     */
    public Result<Polled> poll(OperationDescriptor op, Object args, Object until, Duration interval,
            Duration budget, CallOptions opts) {
        try {
            return Previews.poll(this, op, args, until, interval, budget, opts, null);
        } catch (TungstenException e) {
            return e.toResult();
        }
    }

    /**
     * Runs a compiled macro: its output, or the failing step's envelope with the completed steps'
     * results as the partial value. A destructive or irreversible macro needs a confirmation.
     */
    public Result<Object> runMacro(MacroDescriptor m, Object input, CallOptions opts) {
        try {
            return Macros.run(this, m, input, opts);
        } catch (TungstenException e) {
            return e.toResult();
        }
    }

    /** Previews a macro without sending anything. */
    public Result<PreviewResult> previewMacro(MacroDescriptor m, Object input, CallOptions opts) {
        try {
            return Macros.preview(this, m, input, opts);
        } catch (TungstenException e) {
            return e.toResult();
        }
    }

    /** The server-sent events of an operation; the request is sent by the first read. */
    public EventStream<Object> stream(OperationDescriptor op, Object args, CallOptions opts) {
        return new EventStream<>(this, op, args, opts, v -> v, null);
    }

    /** The authorization-code helpers of an OAuth2 scheme. */
    public OAuthFlow oauth(String scheme) {
        return new OAuthFlow(this, scheme);
    }

    /** The token store of the authorization-code schemes. */
    public TokenStore tokenStore() {
        return tokenStore;
    }

    // ------------------------------------------------------------- internals

    Diag diag(String operation, Category c) {
        return new Diag(identity, operation, c);
    }

    long now() {
        return options.clock != null ? options.clock.getAsLong() : System.currentTimeMillis();
    }

    double random() {
        double v = options.random != null ? options.random.getAsDouble() : Text.randomUnit();
        if (Double.isNaN(v) || Double.isInfinite(v)) {
            return 0.5;
        }
        return Math.max(0, Math.min(1, v));
    }

    /** Reports a warning; a failing observer never changes the call. */
    void emit(Diagnostic d) {
        if (onDiagnostic == null) {
            return;
        }
        try {
            onDiagnostic.accept(d);
        } catch (RuntimeException ignored) {
            // observers never change the call
        }
    }

    Duration attemptTimeout(CallOptions opts) {
        Duration t = opts.timeout();
        if (t == null || t.isZero() || t.isNegative()) {
            t = timeout;
        }
        return t.toMillis() < 1 ? Duration.ofMillis(1) : t;
    }

    static boolean validUrl(String text) {
        try {
            URI u = new URI(text);
            String scheme = u.getScheme();
            return scheme != null
                    && (scheme.equalsIgnoreCase("http") || scheme.equalsIgnoreCase("https"))
                    && u.getRawAuthority() != null
                    && u.getHost() != null
                    && !u.getHost().isEmpty()
                    && u.getRawUserInfo() == null;
        } catch (URISyntaxException e) {
            return false;
        }
    }

    /** The pipeline shared by calls, pages, polls and macro steps. */
    Result.Ok<Object> callWith(OperationDescriptor op, Object args, CallOptions opts, String urlOverride, MacroClaim claim) {
        return callWith(op, args, opts, urlOverride, claim, null);
    }

    /** The request URLs a link-header pagination has used, and the URL that produced the last page. */
    static final class PageTrace {
        final java.util.Set<String> urls = new java.util.HashSet<>();
        String lastUrl;
    }

    /** As {@link #callWith}; with a trace, a request URL used before is refused unsent and the final URL is recorded. */
    Result.Ok<Object> callWith(OperationDescriptor op, Object args, CallOptions opts, String urlOverride, MacroClaim claim,
            PageTrace trace) {
        Prepared p = prepare(op, args, opts, Purpose.CALL, urlOverride, claim);
        if (trace != null && !trace.urls.add(p.url)) {
            throw diag(op.id(), Category.UNEXPECTED_RESPONSE)
                    .remedy("Pagination detected a repeated effective page request URL after authentication. Fix the server's next-page Link before continuing; no repeated request was sent.")
                    .fail();
        }
        Result.Ok<Object> response;
        try {
            response = send(p, opts);
        } catch (TungstenException e) {
            Integer status = e.diagnostic().httpStatus();
            if (status == null || status != 401 || p.oauth == null) {
                throw e;
            }
            response = resendRejected(p, e, opts);
        }
        if (trace != null) {
            trace.lastUrl = p.finalUrl;
            trace.urls.add(p.finalUrl);
        }
        if (opts.verify() && op.agent().verify() != null) {
            Verification v = Previews.verify(this, op, p.args, response.hasValue() ? response.value() : Json.ABSENT, opts);
            response = new Result.Ok<>(response.value(), response.hasValue(), response.raw(), response.meta(), v);
        }
        return response;
    }

    /** Sends a request whose stored OAuth2 token was rejected (401) once more, refreshed. */
    private Result.Ok<Object> resendRejected(Prepared p, TungstenException rejected, CallOptions opts) {
        String scheme = p.oauth;
        p.oauth = null;
        String current = p.headers.get("Authorization");
        if (current == null || !current.startsWith("Bearer ")) {
            throw rejected;
        }
        String token = authResolver.refreshAfterRejection(scheme, current.substring("Bearer ".length()));
        if (token == null) {
            throw rejected;
        }
        p.headers.set("Authorization", "Bearer " + token, true);
        p.secrets.add(token);
        return send(p, opts);
    }

    // --------------------------------------------------------------- prepare

    enum Purpose {
        CALL,
        PREVIEW,
        SERVER_PREVIEW,
        MACRO_PREVIEW
    }

    /** A macro run's one-time spend of its confirmation token: the first step that sends claims it. */
    static final class MacroClaim {
        final String name;
        final String token;
        final String bind;
        final boolean hasBind;
        private final AtomicBoolean claimed = new AtomicBoolean();

        MacroClaim(String name, String token, String bind, boolean hasBind) {
            this.name = name;
            this.token = token;
            this.bind = bind;
            this.hasBind = hasBind;
        }

        void claim(ClientCore c) {
            if (token == null || !claimed.compareAndSet(false, true)) {
                return;
            }
            c.claimToken(name, token, bind, hasBind, "the macro's preview(...)");
        }
    }

    /** A request ready to send, with its redacted rendering. */
    static final class Prepared {
        OperationDescriptor op;
        JsonObject args;
        String method;
        String url;
        /** The URL that answered, after any followed redirect. */
        String finalUrl;
        String displayUrl;
        Serialize.Headers headers;
        Serialize.Payload payload;
        Object displayBody;
        String key;
        boolean hasKey;
        String keyHeader;
        List<Serialize.Pair> authQuery;
        List<String> hiddenQuery;
        List<String> secrets;
        String oauth;
    }

    TungstenException validation(OperationDescriptor op, List<Object> path, Object value, boolean present,
            String expected, String remediation) {
        Object received = present
                ? Text.envelopeValue(Args.redactBelow(op, path, value), Args.sensitiveArg(op, path))
                : Text.envelopeValue(null, Args.sensitiveArg(op, path));
        return diag(op.id(), Category.VALIDATION_FAILED).param(Args.argumentPath(op, path)).received(received)
                .expected(expected).remedy(remediation).fail();
    }

    private static List<Object> keyPath(String name) {
        return List.of(name);
    }

    JsonObject validateArgs(OperationDescriptor op, JsonObject args, boolean placeholders) {
        List<OperationDescriptor.Param> params = op.argumentParams();
        for (OperationDescriptor.Param p : params) {
            if (p.required() && (!args.has(p.name()) || args.get(p.name()) == null)) {
                throw validation(op, keyPath(p.name()), args.get(p.name()), args.has(p.name()),
                        "a value for the required " + p.in() + " parameter " + p.wire(),
                        "Pass " + p.name() + "; " + op.id() + " cannot be called without it.");
            }
        }
        OperationDescriptor.Body body = op.body();
        if (body != null && body.required() && body.arg() != null && !args.has(body.arg())) {
            throw validation(op, keyPath(body.arg()), null, false, "a request body",
                    "Pass the request body as " + body.arg() + ".");
        }
        if (op.request() != null) {
            List<Schema.Issue> issues;
            try {
                issues = op.request().validate(args);
            } catch (RuntimeException e) {
                throw diag(op.id(), Category.UNEXPECTED_RESPONSE).retry(Retryable.NEVER)
                        .remedy("The SDK failed while building or sending the request (the request validator panicked). This is a bug in the generated SDK or the runtime; report it. Nothing was sent.")
                        .fail();
            }
            if (issues.isEmpty()) {
                return args;
            }
            Schema.Issue issue = null;
            if (placeholders) {
                for (Schema.Issue i : issues) {
                    if (!pending(args, i.path())) {
                        issue = i;
                        break;
                    }
                }
                if (issue == null) {
                    return args;
                }
            } else {
                issue = issues.get(0);
            }
            String shown = Args.argumentPath(op, issue.path());
            Object v = Json.at(args, Schema.segments(issue.path()));
            throw validation(op, issue.path(), v == Json.ABSENT ? null : v, v != Json.ABSENT, issue.message(),
                    "Fix " + shown + " (" + issue.message() + ") and call again.");
        }
        // Without a validator, unknown keys are rejected so a misspelt argument is never dropped
        // (rpc methods without a body take free-form args).
        if (op.rpc() != null && body == null) {
            return args;
        }
        Set<String> allowed = new HashSet<>();
        for (OperationDescriptor.Param p : params) {
            allowed.add(p.name());
        }
        if (body != null) {
            if (body.arg() != null) {
                allowed.add(body.arg());
            } else {
                for (OperationDescriptor.MergedField f : body.fields()) {
                    allowed.add(f.arg());
                }
            }
        }
        for (String k : args.keys()) {
            if (allowed.contains(k)) {
                continue;
            }
            List<String> names = new ArrayList<>(allowed);
            names.sort(String::compareTo);
            if (names.size() > 20) {
                names = names.subList(0, 20);
            }
            String list = names.isEmpty() ? "no arguments" : String.join(", ", names);
            throw validation(op, keyPath(k), args.get(k), true, "one of: " + list,
                    "Remove " + k + "; " + op.id() + " does not accept it.");
        }
        return args;
    }

    private static boolean pending(JsonObject args, List<Object> path) {
        List<String> segs = Schema.segments(path);
        for (int i = 0; i < segs.size(); i++) {
            Object v = Json.at(args, segs.subList(0, i + 1));
            if (v != Json.ABSENT && Expr.containsPlaceholder(v)) {
                return true;
            }
        }
        return false;
    }

    // ------------------------------------------------------- idempotency keys

    private static String formatKind(String format) {
        if (format == null) {
            return "token";
        }
        StringBuilder b = new StringBuilder();
        for (char c : format.toLowerCase(Locale.ROOT).toCharArray()) {
            if ((c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')) {
                b.append(c);
            }
        }
        switch (b.toString()) {
            case "uuidv4":
            case "uuid4":
                return "uuid_v4";
            case "uuid":
                return "uuid";
            case "asciitoken128":
                return "ascii_token_128";
            default:
                return "token";
        }
    }

    static String keyFormatDescription(String format) {
        switch (formatKind(format)) {
            case "uuid_v4":
                return "UUIDv4 string";
            case "uuid":
                return "UUID string";
            case "ascii_token_128":
                return "1-128 ASCII letters, digits, dots, underscores or hyphens";
            default:
                return "a non-empty printable ASCII string of at most 255 characters";
        }
    }

    private static boolean isHex(char b) {
        return (b >= '0' && b <= '9') || (b >= 'a' && b <= 'f') || (b >= 'A' && b <= 'F');
    }

    private static boolean isUuid(String key, boolean v4Only) {
        if (key.length() != 36) {
            return false;
        }
        for (int i = 0; i < 36; i++) {
            char b = key.charAt(i);
            boolean ok;
            switch (i) {
                case 8:
                case 13:
                case 18:
                case 23:
                    ok = b == '-';
                    break;
                case 14:
                    ok = v4Only ? b == '4' : b >= '1' && b <= '8';
                    break;
                case 19: {
                    char l = Character.toLowerCase(b);
                    ok = l == '8' || l == '9' || l == 'a' || l == 'b';
                    break;
                }
                default:
                    ok = isHex(b);
            }
            if (!ok) {
                return false;
            }
        }
        return true;
    }

    /** The expected format when the key does not match, else null. */
    static String checkKeyFormat(String key, String format) {
        boolean valid;
        switch (formatKind(format)) {
            case "uuid_v4":
                valid = isUuid(key, true);
                break;
            case "uuid":
                valid = isUuid(key, false);
                break;
            case "ascii_token_128":
                valid = !key.isEmpty() && key.length() <= 128;
                for (int i = 0; i < key.length() && valid; i++) {
                    char c = key.charAt(i);
                    valid = (c >= 'A' && c <= 'Z') || (c >= 'a' && c <= 'z') || (c >= '0' && c <= '9')
                            || c == '.' || c == '_' || c == '-';
                }
                break;
            default:
                valid = !key.isEmpty() && key.length() <= 255;
                for (int i = 0; i < key.length() && valid; i++) {
                    valid = key.charAt(i) >= 0x21 && key.charAt(i) <= 0x7e;
                }
        }
        return valid ? null : keyFormatDescription(format);
    }

    /** The wire header of the operation's key. */
    static String keyHeader(OperationDescriptor op) {
        String h = op.agent().idempotency().header();
        if (h != null && !h.isEmpty()) {
            return h;
        }
        for (OperationDescriptor.Param p : op.params()) {
            if (p.role().equals("idempotency_key")) {
                return p.wire();
            }
        }
        return "Idempotency-Key";
    }

    private void checkKey(OperationDescriptor op, CallOptions opts, Purpose p) {
        OperationDescriptor.Idempotency policy = op.agent().idempotency();
        String note = policy.note() != null && !policy.note().isEmpty() ? " " + policy.note() : "";
        String key = opts.idempotencyKey();
        if (!opts.hasKey()) {
            if (p == Purpose.CALL && policy.policy().equals("caller_owned") && policy.persistRequired()) {
                String fresh = policy.format() != null && policy.format().toLowerCase(Locale.ROOT).contains("uuid") ? "UUIDv4" : "unique key";
                throw diag(op.id(), Category.VALIDATION_FAILED).param(identity.idempotencyKey())
                        .expected(keyFormatDescription(policy.format()))
                        .remedy("Generate a random " + fresh + ", persist it with the intent of this " + op.id()
                                + " call, and pass it as " + identity.idempotencyKey()
                                + ". Reuse the exact same key on every retry; a new key can apply the effect twice." + note)
                        .fail();
            }
            return;
        }
        if (policy.policy().equals("none")) {
            boolean keyed = false;
            for (OperationDescriptor.Param prm : op.params()) {
                if (prm.role().equals("idempotency_key")) {
                    keyed = true;
                }
            }
            if (!keyed) {
                return;
            }
        }
        if (policy.policy().equals("content_identity") || policy.policy().equals("content_hash")) {
            return;
        }
        String format = policy.policy().equals("caller_owned") ? policy.format() : null;
        String expected = checkKeyFormat(key, format);
        if (expected != null) {
            throw diag(op.id(), Category.VALIDATION_FAILED).param(identity.idempotencyKey())
                    .received(Text.envelopeValue(key, false)).expected(expected)
                    .remedy("Pass " + identity.idempotencyKey() + " as " + expected
                            + ". Generate it once, persist it with the intent of this call, and reuse it on every retry." + note)
                    .fail();
        }
    }

    /** [key] or null: the idempotency key this request sends. */
    private String idempotencyKey(OperationDescriptor op, JsonObject args, CallOptions opts, Purpose p, Serialize.Encoded encoded) {
        String supplied = opts.idempotencyKey();
        switch (op.agent().idempotency().policy()) {
            case "content_identity":
                return null;
            case "content_hash":
                return encoded.hashed() == null ? null : Text.sha256Hex(encoded.hashed());
            case "caller_owned":
                return supplied;
            case "auto": {
                if (supplied != null) {
                    return supplied;
                }
                if (p != Purpose.CALL) {
                    return null;
                }
                String logical = Text.sha256Hex(Text.utf8(Json.canonical(args)));
                synchronized (keyLock) {
                    String key;
                    try {
                        key = store.reserve(op.id(), logical, Text.uuidV4());
                    } catch (RuntimeException e) {
                        throw storeFailure(op, "the store threw " + e.getClass().getSimpleName());
                    }
                    if (key == null || key.isEmpty()) {
                        throw storeFailure(op, "the store returned no key");
                    }
                    return key;
                }
            }
            default:
                for (OperationDescriptor.Param prm : op.params()) {
                    if (prm.role().equals("idempotency_key")) {
                        return supplied;
                    }
                }
                return null;
        }
    }

    private TungstenException storeFailure(OperationDescriptor op, String why) {
        return diag(op.id(), Category.TRANSPORT_FAILED).retry(Retryable.NEVER)
                .remedy("The idempotency store failed (" + why
                        + "), so the call was not sent rather than risk a second key for the same intent. Fix or replace ClientOptions.idempotencyStore.")
                .fail();
    }

    // ---------------------------------------------------------- confirmation

    record ConfirmTarget(String id, String subject, String previewCall) {}

    /**
     * The confirmation rule of a tier: destructive accepts YES (unless refused) or a token,
     * irreversible only a token issued for the subject and these exact args.
     */
    void confirmed(ConfirmTarget t, Safety safety, Object args, CallOptions opts) {
        if (safety != Safety.DESTRUCTIVE && safety != Safety.IRREVERSIBLE) {
            return;
        }
        String tier = safety == Safety.DESTRUCTIVE ? "destructive" : "irreversible";
        boolean allowYes = !opts.refuseConfirmYes();
        String how = "call " + t.previewCall() + " with the same arguments and pass its confirmation_token as confirm";
        Confirm c = opts.confirm();
        if (c.isYes()) {
            if (safety == Safety.DESTRUCTIVE && allowYes) {
                return;
            }
            throw required(t, t.id() + " is " + tier + ", so " + identity.confirmYes() + " is not accepted: " + how + ".");
        }
        if (c.tokenValue() != null) {
            switch (Tokens.check(confirmationKey, c.tokenValue(), t.subject(), args, now())) {
                case VALID:
                    return;
                case EXPIRED:
                    throw required(t, "The confirmation token expired (tokens last " + (Tokens.TTL_MS / 60000) + " minutes): " + how + ".");
                default:
                    throw required(t, "The confirmation token was not issued by this client for " + t.id()
                            + " with exactly these arguments: " + how + ".");
            }
        }
        String orYes = safety == Safety.DESTRUCTIVE && allowYes ? " (or pass " + identity.confirmYes() + ")" : "";
        throw required(t, t.id() + " is " + tier + ": " + how + orYes + ".");
    }

    private TungstenException required(ConfirmTarget t, String remediation) {
        return diag(t.id(), Category.CONFIRMATION_REQUIRED).param("confirm")
                .expected("a confirmation_token from preview()").remedy(remediation).fail();
    }

    /**
     * Spends a valid token on the call about to be sent: its first use binds it to that call's
     * replay protection, and a later use is accepted only as a retry under the same protection.
     */
    void claimToken(String id, String token, String bind, boolean hasBind, String previewCall) {
        long now = now();
        synchronized (usedTokens) {
            usedTokens.values().removeIf(u -> u.expiry() <= now);
            UsedToken previous = usedTokens.get(token);
            if (previous != null) {
                if (previous.hasBind() && hasBind && previous.bind().equals(bind)) {
                    return;
                }
                String why;
                if (!previous.hasBind()) {
                    why = "This call has no idempotency key, so a repeat can apply the effect twice";
                } else if (!hasBind) {
                    why = "It was used with an idempotency key; only a retry with that same key may reuse it";
                } else {
                    why = "It was used with another idempotency key; only a retry with that same key may reuse it";
                }
                throw diag(id, Category.CONFIRMATION_REQUIRED).param("confirm")
                        .expected("a confirmation_token from preview()")
                        .remedy("This confirmation token was already used for one " + id + " call. " + why
                                + ". Check whether that call took effect; to send again, call " + previewCall
                                + " and confirm with its new token.")
                        .fail();
            }
            Long expiry = Tokens.expiry(token);
            usedTokens.put(token, new UsedToken(bind, hasBind, expiry != null ? expiry : now + Tokens.TTL_MS));
        }
    }

    // ------------------------------------------------------------- arguments

    static JsonObject normalizeArgs(Object raw) {
        Object converted;
        try {
            converted = Json.normalize(raw);
        } catch (IllegalArgumentException e) {
            return null;
        }
        if (converted == null) {
            return new JsonObject();
        }
        return converted instanceof JsonObject ? (JsonObject) converted : null;
    }

    Prepared prepare(OperationDescriptor op, Object rawArgs, CallOptions opts, Purpose p, String urlOverride, MacroClaim claim) {
        if (op == null) {
            throw diag("<unknown operation>", Category.VALIDATION_FAILED).param("operation")
                    .expected("a valid operation descriptor")
                    .remedy("The operation descriptor is invalid: the descriptor is missing. Regenerate the SDK or fix the descriptor; nothing was sent.")
                    .fail();
        }
        String problem = Args.descriptorProblem(op);
        if (problem != null) {
            throw diag(op.id() == null || op.id().isEmpty() ? "<unknown operation>" : op.id(), Category.VALIDATION_FAILED)
                    .param("operation").expected("a valid operation descriptor")
                    .remedy("The operation descriptor is invalid: " + problem + ". Regenerate the SDK or fix the descriptor; nothing was sent.")
                    .fail();
        }
        register(op);
        JsonObject supplied = normalizeArgs(rawArgs);
        if (supplied == null) {
            List<String> names = new ArrayList<>();
            for (OperationDescriptor.Param prm : op.argumentParams()) {
                if (names.size() == 3) {
                    break;
                }
                names.add(prm.name() + ": ...");
            }
            Object shown;
            try {
                shown = Json.normalize(rawArgs);
            } catch (IllegalArgumentException e) {
                shown = null;
            }
            throw diag(op.id(), Category.VALIDATION_FAILED).param("args").received(Text.envelopeValue(shown, false))
                    .expected("an object of named arguments")
                    .remedy("Pass the arguments of " + op.id() + " as one object, e.g. {" + String.join(", ", names) + "}.")
                    .fail();
        }
        if (Json.nestsDeeperThan(supplied, MAX_ARG_DEPTH)) {
            throw validation(op, List.of(), "<deeply nested value>", true,
                    "a JSON value nested at most " + MAX_ARG_DEPTH + " levels", "Pass arguments without such deep nesting.");
        }
        JsonObject args = validateArgs(op, supplied, p == Purpose.MACRO_PREVIEW);
        checkKey(op, opts, p);
        if (p == Purpose.CALL && claim == null) {
            confirmed(new ConfirmTarget(op.id(), op.id(), "preview(...)"), op.agent().safety(), args, opts);
        }
        String method = op.method();
        Auth.Resolved auth = authResolver.resolve(op, method);
        if (auth.failed != null) {
            throw diag(op.id(), Category.AUTH_FAILED).remedy(auth.failed).fail();
        }
        List<String> secrets = new ArrayList<>();
        for (String s : auth.secrets) {
            addSecret(secrets, s);
        }
        for (OperationDescriptor.Param prm : op.params()) {
            if (!prm.sensitive()) {
                continue;
            }
            Object v = args.get(prm.name());
            if (v instanceof String) {
                addSecret(secrets, (String) v);
            } else if (Json.isNumber(v)) {
                addSecret(secrets, Json.numberText(v));
            }
        }
        for (String path : Args.sensitiveRequestPaths(op)) {
            List<String> found = new ArrayList<>();
            Args.valuesAt(args, Json.splitPath(path), found, 0);
            for (String v : found) {
                addSecret(secrets, v);
            }
        }
        String[] urls = buildUrl(op, args, auth.plan, urlOverride);
        Serialize.Headers hs = new Serialize.Headers();
        buildHeaders(op, args, opts, auth.plan, hs);
        Set<String> paramNames = new HashSet<>();
        for (OperationDescriptor.Param prm : op.argumentParams()) {
            paramNames.add(prm.name());
        }
        List<String> bodyPaths = Args.sensitiveBodyPaths(op);
        Object value = Json.ABSENT;
        if (!Auth.isSafeMethod(method) || method.equals("OPTIONS")) {
            value = Serialize.bodyValue(op, args, paramNames);
        }
        Serialize.Encoded encoded;
        try {
            encoded = Serialize.encode(op.body(), value, display -> {
                for (String bp : bodyPaths) {
                    if (bp.isEmpty()) {
                        return Text.REDACTED;
                    }
                }
                return Text.redactSensitiveKeys(Text.redactPaths(display, bodyPaths), 0);
            });
        } catch (Serialize.Problem e) {
            List<Object> path = op.body() != null && op.body().arg() != null && e.parameter.equals("body")
                    ? keyPath(op.body().arg())
                    : List.of();
            throw validation(op, path, e.value, true, e.expected, "Pass " + e.parameter + " as " + e.expected + ".");
        }
        if (encoded.contentType() != null) {
            hs.set("Content-Type", encoded.contentType(), false);
        }
        String header = keyHeader(op);
        String key = idempotencyKey(op, args, opts, p, encoded);
        String policy = op.agent().idempotency().policy();
        if (key != null) {
            hs.set(header, key, true);
            addSecret(secrets, key);
        } else if (p != Purpose.CALL && (policy.equals("auto") || policy.equals("caller_owned"))) {
            hs.set(header, "<set at call time>", false);
        }
        for (Auth.PlanHeader h : auth.plan.headers) {
            hs.set(h.name(), h.value(), h.secret());
        }
        if (p == Purpose.SERVER_PREVIEW && op.agent().preview().mode().equals("header")) {
            hs.set(op.agent().preview().header(), op.agent().preview().value(), false);
        }
        for (Serialize.Header e : hs.entries) {
            if (Text.validHeaderName(e.name()) && Text.validHeaderValue(e.value())) {
                continue;
            }
            for (Auth.PlanHeader h : auth.plan.headers) {
                if (h.name().equalsIgnoreCase(e.name())) {
                    throw diag(op.id(), Category.AUTH_FAILED)
                            .remedy("The credential for header " + e.name()
                                    + " contains characters not allowed in an HTTP header; check ClientOptions.auth.")
                            .fail();
                }
            }
            List<Object> path = List.of();
            for (OperationDescriptor.Param prm : op.params()) {
                if (prm.wire().equalsIgnoreCase(e.name())) {
                    path = keyPath(prm.name());
                    break;
                }
            }
            throw validation(op, path, e.secret() ? Text.REDACTED : e.value(), true,
                    "a header value of visible ASCII characters (no line breaks)",
                    "Header " + e.name() + " cannot carry this value; remove line breaks and non-Latin-1 characters.");
        }
        if (p == Purpose.CALL) {
            if (claim != null) {
                claim.claim(this);
            } else if (opts.confirm().tokenValue() != null
                    && (op.agent().safety() == Safety.DESTRUCTIVE || op.agent().safety() == Safety.IRREVERSIBLE)) {
                String bind = key;
                boolean hasBind = key != null;
                if (!hasBind && policy.equals("content_identity")) {
                    bind = "content-identity";
                    hasBind = true;
                }
                claimToken(op.id(), opts.confirm().tokenValue(), bind, hasBind, "preview(...)");
            }
        }
        for (String s : hs.secrets()) {
            addSecret(secrets, s);
        }
        List<String> hidden = new ArrayList<>();
        for (Serialize.Pair q : auth.plan.query) {
            hidden.add(q.k());
        }
        for (OperationDescriptor.Param prm : op.params()) {
            if (prm.in().equals("query") && prm.sensitive()) {
                hidden.add(prm.wire());
            }
        }
        Prepared out = new Prepared();
        out.op = op;
        out.args = args;
        out.method = method;
        out.url = urls[0];
        out.displayUrl = urls[1];
        out.headers = hs;
        out.payload = encoded.payload();
        out.displayBody = encoded.display();
        out.key = key;
        out.hasKey = key != null;
        out.keyHeader = header;
        out.authQuery = auth.plan.query;
        out.hiddenQuery = hidden;
        out.secrets = secrets;
        out.oauth = auth.oauth;
        return out;
    }

    static void addSecret(List<String> secrets, String s) {
        if (s != null && !s.isEmpty() && !secrets.contains(s)) {
            secrets.add(s);
        }
    }

    private TungstenException badBase(OperationDescriptor op) {
        return diag(op.id(), Category.TRANSPORT_FAILED).retry(Retryable.NEVER)
                .remedy("The base URL is not a valid absolute http(s) URL without credentials: fix ClientOptions."
                        + identity.baseUrl() + ". Nothing was sent.")
                .fail();
    }

    static boolean isDotSegment(String segment) {
        if (segment.isEmpty()) {
            return true;
        }
        String rest = segment.toLowerCase(Locale.ROOT);
        int count = 0;
        while (count < 3) {
            if (rest.startsWith("%2e")) {
                rest = rest.substring(3);
            } else if (rest.startsWith(".")) {
                rest = rest.substring(1);
            } else {
                break;
            }
            count++;
        }
        return rest.isEmpty() && count >= 1 && count <= 2;
    }

    /** [url as sent, url as shown]. */
    private String[] buildUrl(OperationDescriptor op, JsonObject args, Auth.Plan plan, String urlOverride) {
        if (baseUrl.isEmpty() && urlOverride == null) {
            throw diag(op.id(), Category.TRANSPORT_FAILED).retry(Retryable.NEVER)
                    .remedy("No base URL is configured: set ClientOptions." + identity.baseUrl() + ". Nothing was sent.")
                    .fail();
        }
        List<OperationDescriptor.Param> params = op.argumentParams();
        String full;
        String display;
        if (urlOverride != null) {
            full = urlOverride;
            display = urlOverride;
        } else {
            String unknown = null;
            OperationDescriptor.Param missing = null;
            StringBuilder path = new StringBuilder();
            String rest = op.path();
            while (true) {
                int open = rest.indexOf('{');
                if (open < 0) {
                    break;
                }
                int close = rest.indexOf('}', open);
                if (close < 0) {
                    break;
                }
                String inner = rest.substring(open + 1, close);
                if (inner.isEmpty() || inner.contains("{")) {
                    path.append(rest, 0, open + 1);
                    rest = rest.substring(open + 1);
                    continue;
                }
                path.append(rest, 0, open);
                OperationDescriptor.Param p = null;
                for (OperationDescriptor.Param x : params) {
                    if (x.in().equals("path") && x.wire().equals(inner)) {
                        p = x;
                        break;
                    }
                }
                if (p == null) {
                    for (OperationDescriptor.Param x : params) {
                        if (x.in().equals("path") && x.name().equals(inner)) {
                            p = x;
                            break;
                        }
                    }
                }
                if (p == null) {
                    unknown = inner;
                    path.append(rest, open, close + 1);
                } else if (args.get(p.name()) == null) {
                    missing = p;
                    path.append(rest, open, close + 1);
                } else {
                    path.append(Serialize.pathParam(p, args.get(p.name())));
                }
                rest = rest.substring(close + 1);
            }
            path.append(rest);
            String built = path.toString();
            if (unknown != null) {
                throw diag(op.id(), Category.VALIDATION_FAILED).param("operation").expected("a valid operation descriptor")
                        .remedy("The path template has a placeholder {" + unknown + "} with no path parameter; regenerate the SDK.")
                        .fail();
            }
            if (missing != null) {
                throw validation(op, keyPath(missing.name()), null, false, "a value for path parameter " + missing.wire(),
                        "Pass " + missing.name() + ".");
            }
            String[] template = op.path().split("/", -1);
            String[] segments = built.split("/", -1);
            if (template.length == segments.length) {
                for (int i = 0; i < segments.length; i++) {
                    String tmpl = template[i];
                    if (!tmpl.contains("{") || !isDotSegment(segments[i])) {
                        continue;
                    }
                    String after = tmpl.substring(tmpl.indexOf('{') + 1);
                    String wire = after.contains("}") ? after.substring(0, after.indexOf('}')) : after;
                    OperationDescriptor.Param p = null;
                    for (OperationDescriptor.Param x : params) {
                        if (x.in().equals("path") && (x.wire().equals(wire) || x.name().equals(wire))) {
                            p = x;
                            break;
                        }
                    }
                    Object shownValue = segments[i];
                    List<Object> at = List.of();
                    String name = "the path parameter";
                    if (p != null) {
                        shownValue = args.get(p.name());
                        at = keyPath(p.name());
                        name = p.name();
                    }
                    throw validation(op, at, shownValue, true, "a non-empty path segment other than . and ..",
                            "Pass " + name + " as the identifier of one resource; \"" + segments[i] + "\" would change the request path.");
                }
            }
            List<String> parts = new ArrayList<>();
            List<String> shown = new ArrayList<>();
            for (OperationDescriptor.Param p : params) {
                if (!p.in().equals("query") || !args.has(p.name())) {
                    continue;
                }
                List<String> serialized = Serialize.queryParam(p, args.get(p.name()));
                for (String s : serialized) {
                    shown.add(p.sensitive() ? s.split("=", 2)[0] + "=" + Text.REDACTED : s);
                }
                parts.addAll(serialized);
            }
            for (Serialize.Pair q : plan.query) {
                parts.add(Text.encodeComponent(q.k()) + "=" + Text.encodeComponent(q.v()));
                shown.add(Text.encodeComponent(q.k()) + "=" + Text.REDACTED);
            }
            String root = baseUrl.replaceAll("/+$", "");
            String sep = built.contains("?") ? "&" : "?";
            full = root + built + (parts.isEmpty() ? "" : sep + String.join("&", parts));
            display = root + built + (shown.isEmpty() ? "" : sep + String.join("&", shown));
        }
        if (urlOverride != null && !plan.query.isEmpty()) {
            for (Serialize.Pair q : plan.query) {
                if (!Urls.hasQuery(full, q.k())) {
                    full = Urls.setQuery(full, q.k(), q.v());
                }
                display = Urls.setQuery(display, q.k(), Text.REDACTED);
            }
        }
        if (!validUrl(full)) {
            throw badBase(op);
        }
        return new String[] {full, display};
    }

    private void buildHeaders(OperationDescriptor op, JsonObject args, CallOptions opts, Auth.Plan plan, Serialize.Headers hs) {
        List<String> accept = new ArrayList<>();
        for (OperationDescriptor.Response r : op.responses()) {
            if (r.kind().equals("success") && r.mediaType() != null && !accept.contains(r.mediaType())) {
                accept.add(r.mediaType());
            }
        }
        if (!accept.isEmpty()) {
            hs.set("Accept", String.join(", ", accept), false);
        }
        String apiName = api.name().isEmpty() ? "api" : api.name();
        String apiVersion = api.version().isEmpty() ? "0" : api.version();
        hs.set("X-Tungsten-Runtime", "tungsten-" + identity.runtime() + "/" + identity.version() + " " + apiName + "-sdk/" + apiVersion, false);
        hs.set("X-Tungsten-Operation", op.id(), false);
        String tungsten = api.tungstenVersion().isEmpty() ? identity.version() : api.tungstenVersion();
        hs.set("User-Agent", apiName + "-sdk/" + apiVersion + " tungsten/" + tungsten + " (" + identity.language() + ")", false);
        for (OperationDescriptor.Param p : op.params()) {
            if (p.role().equals("constant") && p.in().equals("header") && p.constant() != null) {
                hs.set(p.wire(), p.constant(), p.sensitive());
            }
        }
        for (Map<String, String> extra : List.of(headers, opts.headers())) {
            for (Map.Entry<String, String> e : new TreeMap<>(extra).entrySet()) {
                hs.set(e.getKey(), e.getValue(), Text.looksSensitive(e.getKey()));
            }
        }
        List<String> cookies = new ArrayList<>();
        String existing = hs.get("Cookie");
        if (existing != null) {
            cookies.add(existing);
        }
        for (OperationDescriptor.Param p : op.argumentParams()) {
            Object v = args.get(p.name());
            if (v == null) {
                continue;
            }
            if (p.in().equals("header")) {
                hs.set(p.wire(), Serialize.headerParam(p, v), p.sensitive());
            } else if (p.in().equals("cookie")) {
                cookies.add(Serialize.cookieParam(p, v));
            }
        }
        for (Serialize.Pair c : plan.cookies) {
            cookies.add(c.k() + "=" + c.v());
        }
        // Cookies are session material: the whole header is always redacted.
        if (!cookies.isEmpty()) {
            hs.set("Cookie", String.join("; ", cookies), true);
        }
    }

    // ------------------------------------------------------------------ send

    /** A decoded response, or an event stream whose body is unread. */
    record Sent(Result.Ok<Object> response, ResponseMeta streamMeta, Transport.Body stream) {}

    Result.Ok<Object> send(Prepared p, CallOptions opts) {
        Sent out = sendWith(p, opts, false);
        if (out.stream() != null) {
            out.stream().close();
            throw diag(p.op.id(), Category.UNEXPECTED_RESPONSE).retry(Retryable.NEVER)
                    .remedy("The SDK received an event stream it did not ask for. This is a bug in the runtime; report it. If the call may have been sent, check its effect before repeating it.")
                    .fail();
        }
        return out.response();
    }

    private record RetryPolicy(int max, double baseMs, double maxMs, RetryOptions.Jitter jitter, boolean honorRetryAfter) {}

    /** The runtime defaults, then the API's defaults for the tier, then the client's, field by field. */
    private RetryPolicy retryPolicy(OperationDescriptor op) {
        int max = 3;
        double baseMs = 200;
        double maxMs = 5000;
        RetryOptions.Jitter jitter = RetryOptions.Jitter.FULL;
        boolean honor = true;
        for (RetryOptions l : List.of(op.isMutation() ? api.mutatingRetries() : api.readOnlyRetries(), retries)) {
            if (l.max() != null) {
                max = Math.max(0, Math.min(100, l.max()));
            }
            if (l.baseMs() != null) {
                baseMs = l.baseMs();
            }
            if (l.maxMs() != null) {
                maxMs = l.maxMs();
            }
            if (l.jitter() != null) {
                jitter = l.jitter();
            }
            if (l.honorRetryAfter() != null) {
                honor = l.honorRetryAfter();
            }
        }
        return new RetryPolicy(max, baseMs, maxMs, jitter, honor);
    }

    private static boolean isRedirect(int status) {
        return status == 301 || status == 302 || status == 303 || status == 307 || status == 308;
    }

    private static boolean retryableCategory(Category c) {
        return c == Category.RATE_LIMITED || c == Category.UPSTREAM_UNAVAILABLE
                || c == Category.TRANSPORT_FAILED || c == Category.OUTCOME_UNKNOWN;
    }

    private Middleware.RequestContext context(Prepared p, int attempt, String method, String url) {
        boolean hasBody = !p.payload.empty;
        return new Middleware.RequestContext(p.op.id(), attempt, method, url, p.headers.toMap(true),
                hasBody ? p.displayBody : null, hasBody);
    }

    private void onRequest(Middleware.RequestContext ctx) {
        for (Middleware m : middleware) {
            try {
                m.onRequest(ctx);
            } catch (RuntimeException ignored) {
                // observers never change the call
            }
        }
    }

    void onResponse(Middleware.RequestContext ctx, int status, Map<String, String> hs) {
        Middleware.ResponseContext r = new Middleware.ResponseContext(status, hs);
        for (Middleware m : middleware) {
            try {
                m.onResponse(ctx, r);
            } catch (RuntimeException ignored) {
                // observers never change the call
            }
        }
    }

    private void onRetry(Middleware.RequestContext ctx, Diagnostic d) {
        for (Middleware m : middleware) {
            try {
                m.onRetry(ctx, d);
            } catch (RuntimeException ignored) {
                // observers never change the call
            }
        }
    }

    /** Sends with retries. With stream, a 2xx text/event-stream answer is returned unread. */
    Sent sendWith(Prepared p, CallOptions opts, boolean stream) {
        OperationDescriptor op = p.op;
        RetryPolicy policy = retryPolicy(op);
        boolean mutation = op.isMutation();
        boolean protectedCall = Classify.hasReplayProtection(op, p.hasKey);
        Classify.OutcomeCheck check = mutation && !protectedCall ? outcomeCheck(op, p.args) : null;
        Duration attemptTimeout = attemptTimeout(opts);
        Duration idle = stream ? idleTimeout : null;
        int attempts = 0;
        while (true) {
            attempts++;
            Middleware.RequestContext rctx = context(p, attempts, p.method, p.displayUrl);
            onRequest(rctx);
            // Redirects are never followed by the HTTP client: it would forward credential headers
            // to another origin. A read follows same-origin redirects here; a mutation none.
            String target = p.url;
            String method = p.method;
            Serialize.Payload body = p.payload;
            List<Serialize.Header> hs = new ArrayList<>(p.headers.entries);
            Transport.Outcome outcome = Transport.attempt(http,
                    new Transport.Request(target, method, hs, body, attemptTimeout, stream, idle));
            for (int hops = 0; !redirectsDisabled && !mutation && hops < MAX_REDIRECTS; hops++) {
                if (outcome.kind != Transport.Kind.RESPONSE || !isRedirect(outcome.status)) {
                    break;
                }
                String[] next = redirectTarget(p, target, outcome.headers.get("location"));
                if (next == null) {
                    break;
                }
                boolean rewrite = outcome.status != 307 && outcome.status != 308 && !method.equals("GET") && !method.equals("HEAD");
                target = next[0];
                if (rewrite) {
                    method = "GET";
                    body = Serialize.Payload.EMPTY;
                    hs.removeIf(h -> h.name().equalsIgnoreCase("content-type"));
                }
                rctx = new Middleware.RequestContext(rctx.operation(), rctx.attempt(), method, next[1], rctx.headers(),
                        rctx.body(), rctx.hasBody());
                outcome = Transport.attempt(http, new Transport.Request(target, method, hs, body, attemptTimeout, stream, idle));
            }
            p.finalUrl = target;
            Classify.Context cctx = new Classify.Context(identity, api, op, p.key, p.hasKey, p.keyHeader, attempts, check);
            TungstenException failure;
            try {
                return classifyResponse(cctx, p, outcome, rctx, attemptTimeout);
            } catch (TungstenException e) {
                failure = e;
            }
            Diagnostic d = failure.diagnostic().scrubbed(p.secrets);
            TungstenException done = new TungstenException(d, failure.partial(), failure.hasPartial());
            if (attempts > policy.max() || !retryableCategory(d.category())) {
                throw done;
            }
            if (d.retryable() == Retryable.NEVER || d.retryable() == Retryable.AFTER_REMEDIATION || (mutation && !protectedCall)) {
                throw done;
            }
            double delayMs;
            if (d.retryAfterMs() != null && policy.honorRetryAfter()) {
                if (d.retryAfterMs() > policy.maxMs()) {
                    throw done;
                }
                delayMs = d.retryAfterMs();
            } else {
                double ceiling = Math.min(policy.maxMs(), policy.baseMs() * Math.pow(2, attempts - 1));
                double r = random();
                switch (policy.jitter()) {
                    case NONE:
                        delayMs = ceiling;
                        break;
                    case EQUAL:
                        delayMs = ceiling / 2 + r * ceiling / 2;
                        break;
                    default:
                        delayMs = r * ceiling;
                }
            }
            onRetry(rctx, d);
            if (!sleep(delayMs)) {
                throw done;
            }
        }
    }

    /** Sleeps; false when the thread was interrupted. */
    static boolean sleep(double ms) {
        if (!(ms > 0)) {
            return true;
        }
        try {
            double capped = Math.min(ms, 4.0e9);
            long whole = (long) Math.floor(capped);
            Thread.sleep(whole, (int) Math.min(999999, (capped - whole) * 1e6));
            return true;
        } catch (InterruptedException e) {
            Thread.currentThread().interrupt();
            return false;
        }
    }

    /** [sent, shown] of a read's redirect that stays on the request's origin, or null. */
    private static String[] redirectTarget(Prepared p, String from, String location) {
        if (location == null || location.isEmpty()) {
            return null;
        }
        URI target = Urls.resolve(from, location);
        if (target == null || !Urls.sameOrigin(URI.create(from), target) || target.getRawUserInfo() != null) {
            return null;
        }
        String sent = target.toString();
        for (Serialize.Pair q : p.authQuery) {
            if (!Urls.hasQuery(sent, q.k())) {
                sent = Urls.setQuery(sent, q.k(), q.v());
            }
        }
        String shown = sent;
        for (String name : p.hiddenQuery) {
            if (Urls.hasQuery(shown, name)) {
                shown = Urls.setQuery(shown, name, Text.REDACTED);
            }
        }
        return new String[] {sent, shown};
    }

    private Sent classifyResponse(Classify.Context cctx, Prepared p, Transport.Outcome outcome,
            Middleware.RequestContext rctx, Duration attemptTimeout) {
        OperationDescriptor op = p.op;
        boolean mutation = op.isMutation();
        int attempts = cctx.attempts();
        switch (outcome.kind) {
            case NOT_SENT:
                throw diag(op.id(), Category.TRANSPORT_FAILED)
                        .remedy("The request could not be delivered (" + outcome.detail + "), so the server did not receive it. Check "
                                + identity.baseUrl() + " and network access, then call again.")
                        .attempts(attempts).fail();
            case LOST: {
                String cause = "The connection failed before a response arrived (" + outcome.detail + ").";
                if (mutation) {
                    throw new TungstenException(Classify.outcomeUnknown(cctx, cause, null, null, null, null, null));
                }
                throw diag(op.id(), Category.TRANSPORT_FAILED).remedy(cause + " This read has no side effects; call again.")
                        .attempts(attempts).fail();
            }
            case ABORTED:
                if (mutation && outcome.sent) {
                    throw new TungstenException(Classify.outcomeUnknown(cctx,
                            "The caller's thread was interrupted after the call was sent.", null, null, null, null, null));
                }
                throw diag(op.id(), Category.TRANSPORT_FAILED).retry(Retryable.NEVER)
                        .remedy("The caller's thread was interrupted" + (outcome.sent ? "" : " before the call was sent") + ".")
                        .attempts(attempts).fail();
            case TIMEOUT: {
                long ms = attemptTimeout.toMillis();
                if (mutation) {
                    throw new TungstenException(Classify.outcomeUnknown(cctx, "No response arrived within " + ms + " ms.", null, null, null, null, null));
                }
                throw diag(op.id(), Category.UPSTREAM_UNAVAILABLE)
                        .remedy("No response arrived within " + ms + " ms. This read has no side effects; call again later or with a larger "
                                + identity.timeout() + ".")
                        .attempts(attempts).fail();
            }
            case STREAMING:
                onResponse(rctx, outcome.status, outcome.headers);
                return new Sent(null, new ResponseMeta(outcome.status, outcome.headers, Classify.requestId(outcome.headers), attempts), outcome.stream);
            default:
                break;
        }
        int status = outcome.status;
        Map<String, String> hs = outcome.headers;
        onResponse(rctx, status, hs);
        String requestId = Classify.requestId(hs);
        Long retryAfter = Text.parseRetryAfter(hs.get("retry-after"), now());
        OperationDescriptor.Response declared = Classify.match(op.responses(), status);
        // A status the descriptor declares (exactly or by class) is classified as declared. With
        // redirects disabled, a 3xx it does not declare is a refused redirect, never a success.
        boolean refusedRedirect = redirectsDisabled && status >= 300 && status <= 399
                && (declared == null || declared.statusKind().equals("default"));
        boolean success = !refusedRedirect && ((status >= 200 && status <= 299)
                || (declared != null && declared.kind().equals("success") && status >= 100 && status <= 399));
        String hint = op.agent().verify() != null ? "Call " + op.agent().verify().operation() + " to read the current state." : null;
        if (success && !outcome.bodyOk) {
            String failure = "timeout".equals(outcome.bodyFailure) ? "timeout" : "broken";
            if (!mutation) {
                throw diag(op.id(), Category.UPSTREAM_UNAVAILABLE).status(status).requestId(requestId).retry(Retryable.AFTER_DELAY)
                        .remedy("The response body was cut off (" + failure + "). This read has no side effects; call again.")
                        .attempts(attempts).fail();
            }
            throw diag(op.id(), Category.UNEXPECTED_RESPONSE).status(status).requestId(requestId).retry(Retryable.NEVER)
                    .remedy("The server accepted " + op.id() + " (HTTP " + status + ") but the response body was cut off (" + failure
                            + "). The call took effect; do not repeat it.")
                    .next(hint).attempts(attempts).fail();
        }
        byte[] data = outcome.body == null ? new byte[0] : outcome.body;
        String declaredMedia = declared != null ? declared.mediaType() : null;
        if (!success) {
            if (status >= 300 && status <= 399) {
                String to = hs.containsKey("location") ? " to " + Text.takeUnits(hs.get("location"), 200) : "";
                String remediation;
                String next = null;
                if (mutation) {
                    remediation = "The server answered with a redirect" + to + ". Redirects are never followed on mutations; check "
                            + identity.baseUrl() + " (scheme, host, trailing slash). Whether the call took effect is unknown only if the server applied it before redirecting.";
                    next = hint;
                } else if (redirectsDisabled) {
                    remediation = "The server answered with a redirect" + to + ". Redirects are disabled; check redirects and "
                            + identity.baseUrl() + ".";
                } else {
                    remediation = "The server answered with a redirect" + to
                            + " that leaves the API's origin (or too many redirects). It is not followed, so credentials never leave the API's host; check "
                            + identity.baseUrl() + ".";
                }
                throw diag(op.id(), Category.UNEXPECTED_RESPONSE).status(status).requestId(requestId).remedy(remediation)
                        .next(next).attempts(attempts).fail();
            }
            Classify.Decoded decoded = Classify.decode(data, hs, declaredMedia);
            throw new TungstenException(Classify.error(cctx, status, hs, decoded, retryAfter));
        }
        Classify.Decoded decoded = Classify.decode(data, hs, declaredMedia);
        String afterEffect = mutation ? " The call took effect; do not repeat it." : "";
        Diagnostic problem = null;
        if (decoded.invalidJson) {
            String expected = "a JSON body";
            String remediation = "The success response announced JSON but did not parse." + afterEffect;
            if (decoded.jsonl) {
                expected = "a JSON Lines body";
                remediation = "The success response announced JSON Lines but line " + decoded.badLine + " did not parse." + afterEffect;
            }
            problem = diag(op.id(), Category.UNEXPECTED_RESPONSE).status(status).requestId(requestId).param("response")
                    .expected(expected).remedy(remediation).attempts(attempts).build();
        } else if (validateResponses != ClientOptions.ValidateResponses.OFF && op.response() != null && decoded.has) {
            List<Schema.Issue> issues = judge(op.response(), decoded.value);
            if (!issues.isEmpty()) {
                Schema.Issue issue = issues.get(0);
                List<String> path = Schema.segments(issue.path());
                String pathText = Schema.formatPath(issue.path());
                String kept = mutation && validateResponses == ClientOptions.ValidateResponses.STRICT
                        ? " The decoded body is in the result's partial; store any value shown only once from it before anything else."
                        : "";
                Object redacted = Text.redactPaths(decoded.value, op.agent().sensitiveResponseFields());
                Object received = Json.at(redacted, path);
                boolean sensitive = false;
                for (String s : path) {
                    sensitive |= Text.looksSensitive(s);
                }
                problem = diag(op.id(), Category.UNEXPECTED_RESPONSE).status(status).requestId(requestId)
                        .param("response" + pathText)
                        .received(Text.envelopeValue(received == Json.ABSENT ? null : received, sensitive))
                        .expected(issue.message())
                        .remedy("The success response does not match the API description at response" + pathText + " ("
                                + issue.message() + ")." + afterEffect + kept)
                        .attempts(attempts).build();
            }
        }
        if (problem != null) {
            Diagnostic scrubbed = problem.scrubbed(p.secrets);
            if (validateResponses == ClientOptions.ValidateResponses.STRICT) {
                // The effect of a mutation happened: its body is never dropped, since it can hold
                // the only copy of a value.
                if (mutation && !decoded.invalidJson) {
                    throw new TungstenException(scrubbed, decoded.value, decoded.has);
                }
                throw new TungstenException(scrubbed);
            }
            emit(scrubbed);
        }
        ResponseMeta meta = new ResponseMeta(status, hs, requestId, attempts);
        return new Sent(new Result.Ok<>(decoded.value, decoded.has, decoded.value, meta, null), null, null);
    }

    /** Runs a validator that only judges: a failure is an issue at the root. */
    static List<Schema.Issue> judge(Schema s, Object value) {
        try {
            return s.validate(value);
        } catch (RuntimeException e) {
            return List.of(new Schema.Issue(List.of(), "the response schema failed (the validator threw)"));
        }
    }

    // --------------------------------------------------------- outcome check

    /** How to find out whether a mutation without replay protection took effect, or null. */
    Classify.OutcomeCheck outcomeCheck(OperationDescriptor op, JsonObject args) {
        String sentArgs = Args.sentArguments(op, args);
        OperationDescriptor.Verify hook = op.agent().verify();
        if (hook != null && !hook.operation().isEmpty() && Classify.verifyCallable(hook)) {
            JsonObject sc = new JsonObject();
            sc.set("args", Args.withWireNames(op, args));
            Object hookArgs = hook.args() != null ? hook.args() : new JsonObject();
            Object evaluated = Expr.evaluate(hookArgs, sc, 0);
            String call = hook.operation() + Args.withArguments(evaluated);
            List<String> lost = new ArrayList<>();
            Object expect = new JsonObject();
            if (hook.expect() instanceof JsonObject) {
                expect = resolveLost(hook.expect(), sc, lost, 0);
            }
            List<String> fields = expect instanceof JsonObject ? ((JsonObject) expect).keys() : List.of();
            String shows;
            if (fields.isEmpty()) {
                shows = "whether it shows " + Classify.changeOf(op);
            } else if (!lost.isEmpty()) {
                List<String> unique = new ArrayList<>();
                for (String item : lost) {
                    if (!unique.contains(item)) {
                        unique.add(item);
                    }
                }
                shows = "whether " + String.join(" and ", fields) + " holds what " + op.id()
                        + " creates, matching the arguments you sent" + sentArgs + " (its " + String.join(", ", unique)
                        + " was in the lost response)";
            } else {
                shows = "whether " + Expr.describe(expect);
            }
            return new Classify.OutcomeCheck(call, shows);
        }
        Object[] read = resourceRead(op, args);
        if (read == null) {
            return null;
        }
        return new Classify.OutcomeCheck(read[0] + Args.withArguments(read[1]), "whether it shows " + Classify.changeOf(op));
    }

    private static boolean isRootSegment(String s) {
        if (s.isEmpty() || s.equals("api")) {
            return true;
        }
        if (s.length() > 1 && (s.charAt(0) == 'v' || s.charAt(0) == 'V')) {
            for (String n : s.substring(1).split("\\.", -1)) {
                if (!Text.allDigits(n)) {
                    return false;
                }
            }
            return true;
        }
        return false;
    }

    /**
     * [id, args] of a registered read of the resource op changes: a GET whose path is the
     * longest prefix of op's path, never the API root, whose required parameters are path
     * parameters op was called with.
     */
    private Object[] resourceRead(OperationDescriptor op, JsonObject args) {
        if (op.rpc() != null) {
            return null;
        }
        Map<String, Object> known = new LinkedHashMap<>();
        for (OperationDescriptor.Param p : op.params()) {
            if (p.in().equals("path") && args.get(p.name()) != null) {
                known.put(p.wire(), args.get(p.name()));
            }
        }
        String[] segments = op.path().split("/", -1);
        List<OperationDescriptor> reads = new ArrayList<>();
        synchronized (registry) {
            for (OperationDescriptor r : new TreeMap<>(registry).values()) {
                if (!r.id().equals(op.id()) && r.method().equals("GET") && r.agent().safety() == Safety.READ_ONLY && r.rpc() == null) {
                    reads.add(r);
                }
            }
        }
        for (int end = segments.length; end >= 1; end--) {
            boolean allRoot = true;
            for (int i = 0; i < end; i++) {
                if (!isRootSegment(segments[i])) {
                    allRoot = false;
                    break;
                }
            }
            if (allRoot) {
                break;
            }
            String path = String.join("/", java.util.Arrays.copyOfRange(segments, 0, end));
            for (OperationDescriptor read : reads) {
                if (!read.path().equals(path)) {
                    continue;
                }
                boolean requiredOk = true;
                for (OperationDescriptor.Param p : read.argumentParams()) {
                    if (p.required() && (!p.in().equals("path") || !known.containsKey(p.wire()))) {
                        requiredOk = false;
                    }
                }
                if (!requiredOk) {
                    continue;
                }
                JsonObject readArgs = new JsonObject();
                for (OperationDescriptor.Param p : read.argumentParams()) {
                    if (p.in().equals("path") && known.containsKey(p.wire())) {
                        readArgs.set(p.wire(), known.get(p.wire()));
                    }
                }
                return new Object[] {read.id(), readArgs};
            }
        }
        return null;
    }

    /**
     * The node resolved against the call's arguments; references to the lost response show as
     * &lt;response...&gt; and are recorded in lost.
     */
    private static Object resolveLost(Object node, JsonObject sc, List<String> lost, int depth) {
        if (depth > 32) {
            return null;
        }
        if (node instanceof String) {
            String t = (String) node;
            if (t.startsWith("$response")) {
                String rest = t.substring("$response".length());
                if (rest.startsWith(".")) {
                    rest = rest.substring(1);
                }
                lost.add(rest.isEmpty() ? "body" : rest);
                return "<" + t.substring(1) + ">";
            }
            if (t.startsWith("$")) {
                Object v = Expr.ref(t, sc);
                return v != Json.ABSENT ? v : "<" + t.substring(1) + ">";
            }
            return t;
        }
        if (node instanceof List<?>) {
            List<Object> out = new ArrayList<>();
            for (Object item : (List<?>) node) {
                out.add(resolveLost(item, sc, lost, depth + 1));
            }
            return out;
        }
        if (node instanceof JsonObject) {
            JsonObject t = (JsonObject) node;
            JsonObject out = new JsonObject();
            for (String k : t.keys()) {
                out.set(k, resolveLost(t.get(k), sc, lost, depth + 1));
            }
            return out;
        }
        return node;
    }
}
