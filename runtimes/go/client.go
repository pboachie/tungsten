// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"encoding/binary"
	"math"
	"net/http"
	"net/url"
	"strings"
	"sync"
	"time"
)

// Version of this runtime, sent as X-Tungsten-Runtime: tungsten-go/<version>.
const Version = "0.1.0"

// ClientCore is the engine behind every generated client: pre-flight
// validation, auth, idempotency keys, confirmation rules, sending with
// tier-aware retries and classification into a result or the error
// envelope. It is safe for concurrent use. API and transport failures are
// returned as *Error values carrying the envelope.
type ClientCore struct {
	api               *APIDescriptor
	auth              AuthConfig
	headers           map[string]string
	baseURL           string
	timeout           time.Duration
	maxEventBytes     int
	maxCollectBytes   int
	maxCollectTime    time.Duration
	retries           PartialRetry
	store             IdempotencyStore
	middleware        []Middleware
	validateResponses ValidateResponses
	onDiagnostic      func(*Diagnostic)
	confirmationKey   []byte
	http              *http.Client
	nowFn             func() int64
	randomFn          func() float64
	tokenStore        TokenStore

	mu         sync.Mutex
	registry   map[string]*OperationDescriptor
	usedTokens map[string]usedToken
	oauthGates map[string]*gate
	// keyMu makes "find or create the key of a logical call" atomic.
	keyMu    sync.Mutex
	ccMu     sync.Mutex
	ccTokens map[string]cachedToken
}

type usedToken struct {
	bind    string
	hasBind bool
	expiry  int64
}

// ConfigError reports a client that could not be built.
type ConfigError struct {
	Message string
}

func (e *ConfigError) Error() string { return e.Message }

func validBaseURL(base string) bool {
	u, err := url.Parse(base)
	return err == nil && (u.Scheme == "http" || u.Scheme == "https") && u.Host != "" && u.User == nil && u.Hostname() != ""
}

// NewClientCore builds a client.
func NewClientCore(api *APIDescriptor, options ClientOptions) (*ClientCore, error) {
	if api == nil {
		return nil, &ConfigError{Message: "the API descriptor is missing"}
	}
	base := options.BaseURL
	if base == "" && len(api.Servers) > 0 {
		base = api.Servers[0]
	}
	if base != "" && !validBaseURL(base) {
		return nil, &ConfigError{Message: "the base URL is not a valid absolute http(s) URL without credentials"}
	}
	key := options.ConfirmationKey
	if len(key) == 0 {
		random, ok := randomBytes(32)
		if !ok {
			return nil, &ConfigError{Message: "no source of randomness for the confirmation key"}
		}
		key = random
	}
	c := &ClientCore{
		api:               api,
		auth:              options.Auth,
		headers:           options.Headers,
		baseURL:           base,
		timeout:           options.Timeout,
		maxEventBytes:     options.MaxEventBytes,
		maxCollectBytes:   options.MaxCollectBytes,
		maxCollectTime:    options.MaxCollectTime,
		retries:           options.Retries,
		store:             options.IdempotencyStore,
		middleware:        options.Middleware,
		validateResponses: options.ValidateResponses,
		onDiagnostic:      options.OnDiagnostic,
		confirmationKey:   key,
		http:              options.HTTPClient,
		nowFn:             options.Now,
		randomFn:          options.Random,
		tokenStore:        options.TokenStore,
		registry:          map[string]*OperationDescriptor{},
		usedTokens:        map[string]usedToken{},
		oauthGates:        map[string]*gate{},
		ccTokens:          map[string]cachedToken{},
	}
	if c.auth == nil {
		c.auth = AuthConfig{}
	}
	if c.timeout <= 0 {
		c.timeout = DefaultTimeout
	}
	if c.maxEventBytes <= 0 {
		c.maxEventBytes = DefaultMaxEventBytes
	}
	if c.maxCollectBytes <= 0 {
		c.maxCollectBytes = DefaultMaxCollectBytes
	}
	if c.maxCollectTime <= 0 {
		c.maxCollectTime = DefaultMaxCollectTime
	}
	if c.store == nil {
		c.store = NewMemoryIdempotencyStore()
	}
	if c.validateResponses == "" {
		c.validateResponses = ValidateWarn
	}
	if c.http == nil {
		c.http = newHTTPClient()
	}
	if c.tokenStore == nil {
		c.tokenStore = NewMemoryTokenStore()
	}
	c.Register(options.Operations...)
	return c, nil
}

// API returns the API descriptor.
func (c *ClientCore) API() *APIDescriptor { return c.api }

// Register makes operations resolvable by id (verification, endpoint
// previews, macros). Operations passed to Call are registered too.
func (c *ClientCore) Register(ops ...*OperationDescriptor) {
	c.mu.Lock()
	defer c.mu.Unlock()
	for _, op := range ops {
		if op != nil && op.ID != "" {
			if _, ok := c.registry[op.ID]; !ok {
				c.registry[op.ID] = op
			}
		}
	}
}

// Operation returns a registered operation.
func (c *ClientCore) Operation(id string) (*OperationDescriptor, bool) {
	c.mu.Lock()
	defer c.mu.Unlock()
	op, ok := c.registry[id]
	return op, ok
}

// Call validates, authenticates, applies idempotency and confirmation
// rules, sends with retries and classifies the response. args is the
// arguments object (keyed by argument name); a nil args is no arguments.
func (c *ClientCore) Call(ctx context.Context, op *OperationDescriptor, args any, opts CallOptions) (*Outcome, error) {
	out, err := c.callWith(ctx, op, args, opts, "", nil)
	if err != nil {
		return nil, err
	}
	return out, nil
}

// Preview renders the request without sending it (or runs the operation's
// header or endpoint preview). Mutating operations get a confirmation
// token bound to these exact arguments for five minutes.
func (c *ClientCore) Preview(ctx context.Context, op *OperationDescriptor, args any, opts CallOptions) (*Response[*PreviewResult], error) {
	out, err := c.preview(ctx, op, args, opts)
	if err != nil {
		return nil, err
	}
	return out, nil
}

// Pages iterates the pages of a paginated operation.
func (c *ClientCore) Pages(op *OperationDescriptor, args any, opts CallOptions) *Pages {
	return &Pages{core: c, op: op, opts: opts, state: newPageState(args)}
}

// Polled is the body a poll ended with.
type Polled struct {
	Body     any
	HasBody  bool
	TimedOut bool
}

// Poll calls a read operation until until holds or the budget runs out; on
// a timeout the last body is returned with TimedOut true.
func (c *ClientCore) Poll(ctx context.Context, op *OperationDescriptor, args any, until any, interval, budget time.Duration, opts CallOptions) (*Response[Polled], error) {
	out, err := c.pollWith(ctx, op, args, pollSpec{until: until, interval: interval, budget: budget}, opts, nil)
	if err != nil {
		return nil, err
	}
	return out, nil
}

// RunMacro runs a compiled macro: its output, or the failing step's
// envelope with the completed steps' results as Partial. A destructive or
// irreversible macro needs opts.Confirm.
func (c *ClientCore) RunMacro(ctx context.Context, m *MacroDescriptor, input any, opts CallOptions) (*Outcome, error) {
	out, err := c.runMacro(ctx, m, input, opts)
	if err != nil {
		return nil, err
	}
	return out, nil
}

// PreviewMacro previews a macro without sending anything.
func (c *ClientCore) PreviewMacro(ctx context.Context, m *MacroDescriptor, input any, opts CallOptions) (*Response[*PreviewResult], error) {
	out, err := c.previewMacro(ctx, m, input, opts)
	if err != nil {
		return nil, err
	}
	return out, nil
}

// callWith is the pipeline shared by calls, pages, polls and macro steps.
func (c *ClientCore) callWith(ctx context.Context, op *OperationDescriptor, args any, opts CallOptions, urlOverride string, claim *macroClaim) (*Outcome, *Error) {
	prepared, err := c.prepare(ctx, op, args, opts, purposeCall, urlOverride, claim)
	if err != nil {
		return nil, err
	}
	response, err := c.send(ctx, prepared, opts)
	if err != nil {
		if err.Diagnostic.HTTPStatus != nil && *err.Diagnostic.HTTPStatus == 401 && prepared.oauth != "" {
			response, err = c.resendRejected(ctx, prepared, err, opts)
		}
		if err != nil {
			return nil, err
		}
	}
	if opts.Verify && op.Agent.Verify != nil {
		v := c.verify(ctx, op, prepared.args, response.Value, response.HasValue, opts)
		response.Verification = &v
	}
	return response, nil
}

// resendRejected sends a request whose stored OAuth2 token was rejected
// (401) once more with the refreshed token.
func (c *ClientCore) resendRejected(ctx context.Context, p *prepared, rejected *Error, opts CallOptions) (*Outcome, *Error) {
	scheme := p.oauth
	p.oauth = ""
	current, ok := p.headers.get("Authorization")
	if !ok || !strings.HasPrefix(current, "Bearer ") {
		return nil, rejected
	}
	token, ok := c.refreshAfterRejection(ctx, scheme, strings.TrimPrefix(current, "Bearer "))
	if !ok {
		return nil, rejected
	}
	p.headers.set("Authorization", "Bearer "+token, true)
	p.secrets.insert(token)
	return c.send(ctx, p, opts)
}

func (c *ClientCore) now() int64 {
	if c.nowFn != nil {
		return c.nowFn()
	}
	return time.Now().UnixMilli()
}

func (c *ClientCore) random() float64 {
	var v float64
	if c.randomFn != nil {
		v = c.randomFn()
	} else if b, ok := randomBytes(8); ok {
		v = float64(binary.LittleEndian.Uint64(b)>>11) / float64(uint64(1)<<53)
	} else {
		v = 0.5
	}
	if math.IsNaN(v) || math.IsInf(v, 0) {
		return 0.5
	}
	return math.Max(0, math.Min(1, v))
}

// retry is the retry policy of one operation after merging the layers.
type retry struct {
	max             int
	baseMs          float64
	maxMs           float64
	jitter          Jitter
	honorRetryAfter bool
}

// retryOptions: the runtime defaults, then the API's defaults for the
// operation's tier, then ClientOptions.Retries, field by field.
func (c *ClientCore) retryOptions(op *OperationDescriptor) retry {
	out := retry{max: 3, baseMs: 200, maxMs: 5000, jitter: JitterFull, honorRetryAfter: true}
	var layers []PartialRetry
	if c.api.Retries != nil {
		if isMutation(op) {
			layers = append(layers, c.api.Retries.Mutating)
		} else {
			layers = append(layers, c.api.Retries.ReadOnly)
		}
	}
	layers = append(layers, c.retries)
	for _, l := range layers {
		if l.Max != nil {
			out.max = *l.Max
			if out.max > 100 {
				out.max = 100
			}
			if out.max < 0 {
				out.max = 0
			}
		}
		if l.Base != nil {
			out.baseMs = float64(*l.Base) / float64(time.Millisecond)
		}
		if l.MaxDelay != nil {
			out.maxMs = float64(*l.MaxDelay) / float64(time.Millisecond)
		}
		if l.Jitter != "" {
			out.jitter = l.Jitter
		}
		if l.HonorRetryAfter != nil {
			out.honorRetryAfter = *l.HonorRetryAfter
		}
	}
	return out
}

// attemptTimeout is the per-attempt timeout (at least one millisecond).
func (c *ClientCore) attemptTimeout(opts CallOptions) time.Duration {
	t := opts.Timeout
	if t <= 0 {
		t = c.timeout
	}
	if t < time.Millisecond {
		t = time.Millisecond
	}
	return t
}

// emit reports a warning; a failing observer never changes the call.
func (c *ClientCore) emit(d *Diagnostic) {
	if c.onDiagnostic == nil {
		return
	}
	defer func() { _ = recover() }()
	c.onDiagnostic(d)
}
