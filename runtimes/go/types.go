// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"fmt"
	"net/http"
	"strings"
	"time"
)

// ---------------------------------------------------------------- errors

// Safety is the tier of an operation.
type Safety string

const (
	ReadOnly     Safety = "read_only"
	Mutating     Safety = "mutating"
	Destructive  Safety = "destructive"
	Irreversible Safety = "irreversible"
)

// Retryable says whether and how a failed call may be repeated.
type Retryable string

const (
	RetryNever            Retryable = "never"
	RetryAfterDelay       Retryable = "after_delay"
	RetrySameKeyOnly      Retryable = "same_key_only"
	RetryAfterRemediation Retryable = "after_remediation"
)

// Category is the closed set of error categories shared by every tungsten
// runtime.
type Category string

const (
	ValidationFailed     Category = "VALIDATION_FAILED"
	MalformedRequest     Category = "MALFORMED_REQUEST"
	RequestTooLarge      Category = "REQUEST_TOO_LARGE"
	AuthFailed           Category = "AUTH_FAILED"
	NotFound             Category = "NOT_FOUND"
	Conflict             Category = "CONFLICT"
	PreconditionFailed   Category = "PRECONDITION_FAILED"
	RateLimited          Category = "RATE_LIMITED"
	UpstreamUnavailable  Category = "UPSTREAM_UNAVAILABLE"
	OutcomeUnknown       Category = "OUTCOME_UNKNOWN"
	TransportFailed      Category = "TRANSPORT_FAILED"
	ConfirmationRequired Category = "CONFIRMATION_REQUIRED"
	GateDisabled         Category = "GATE_DISABLED"
	UnexpectedResponse   Category = "UNEXPECTED_RESPONSE"
)

// Categories lists every category.
var Categories = []Category{
	ValidationFailed, MalformedRequest, RequestTooLarge, AuthFailed, NotFound, Conflict,
	PreconditionFailed, RateLimited, UpstreamUnavailable, OutcomeUnknown, TransportFailed,
	ConfirmationRequired, GateDisabled, UnexpectedResponse,
}

// Diagnostic is the error envelope. It serializes to exactly the fourteen
// members of specs/envelope.schema.json, in order, every member present
// (null when it has no value), identical across the runtimes.
type Diagnostic struct {
	Category  Category
	Operation string
	// HTTPStatus, Code, FailedParameter, Expected, RetryAfterMs, NextAction
	// and RequestID are nil when they have no value.
	HTTPStatus      *int
	Code            *string
	FailedParameter *string
	// ReceivedValue is redacted when sensitive; strings cut to 200 chars.
	ReceivedValue any
	Expected      *string
	Remediation   string
	Retryable     Retryable
	RetryAfterMs  *int64
	NextAction    *string
	RequestID     *string
	Attempts      int
}

func optString(p *string) any {
	if p == nil {
		return nil
	}
	return *p
}

// Object is the envelope as a JSON object in the documented member order.
func (d *Diagnostic) Object() *Object {
	o := NewObject()
	o.Set("status", "error")
	o.Set("category", string(d.Category))
	o.Set("operation", d.Operation)
	if d.HTTPStatus != nil {
		o.Set("http_status", int64(*d.HTTPStatus))
	} else {
		o.Set("http_status", nil)
	}
	o.Set("code", optString(d.Code))
	o.Set("failed_parameter", optString(d.FailedParameter))
	o.Set("received_value", d.ReceivedValue)
	o.Set("expected", optString(d.Expected))
	o.Set("remediation", d.Remediation)
	o.Set("retryable", string(d.Retryable))
	if d.RetryAfterMs != nil {
		o.Set("retry_after_ms", *d.RetryAfterMs)
	} else {
		o.Set("retry_after_ms", nil)
	}
	o.Set("next_action", optString(d.NextAction))
	o.Set("request_id", optString(d.RequestID))
	trace := NewObject()
	trace.Set("attempts", int64(d.Attempts))
	o.Set("trace", trace)
	return o
}

// MarshalJSON writes the envelope.
func (d *Diagnostic) MarshalJSON() ([]byte, error) {
	return []byte(JSONText(d.Object())), nil
}

// Error is "<category> in <operation>: <remediation>".
func (d *Diagnostic) Error() string {
	return fmt.Sprintf("%s in %s: %s", d.Category, d.Operation, d.Remediation)
}

// Error is a failed call: the envelope, plus what the failed call or macro
// already produced when its effect happened (the success body of a mutation
// whose response failed strict validation, or a macro's completed step
// results by their "as" names). Partial can hold values the API shows only
// once, so store them before acting on the error; they are never repeated
// in the envelope or in Error().
type Error struct {
	Diagnostic *Diagnostic
	// Partial is meaningful when HasPartial is true.
	Partial    any
	HasPartial bool
}

func (e *Error) Error() string { return e.Diagnostic.Error() }

// Unwrap returns the envelope.
func (e *Error) Unwrap() error { return e.Diagnostic }

func newError(d *Diagnostic) *Error { return &Error{Diagnostic: d} }

// ResponseMeta describes the HTTP answer.
type ResponseMeta struct {
	Status int
	// Headers have lower-cased names; repeated values are joined with ", "
	// (set-cookie keeps its last value).
	Headers   map[string]string
	RequestID *string
	Attempts  int
}

// Verification is the result of an operation's verification hook.
type Verification struct {
	Checked bool
	// Passed is true when expect held.
	Passed      bool
	Observed    any
	HasObserved bool
	// TimedOut is true when polling for terminal ran out of budget.
	TimedOut bool
	// Error is why the check could not run; Checked is false.
	Error *Diagnostic
}

// Response is a successful call. A success without a body (204, or a
// status declared without content) has HasValue false at the dynamic level.
type Response[T any] struct {
	Value T
	// HasValue is false when the response had no body.
	HasValue bool
	// Raw is the decoded wire body (typed results keep it too).
	Raw          any
	Meta         ResponseMeta
	Verification *Verification
}

// Outcome is the result of a dynamic call: the decoded JSON body.
type Outcome = Response[any]

// ----------------------------------------------------------- descriptors

// Location is where a parameter travels.
type Location string

const (
	InPath   Location = "path"
	InQuery  Location = "query"
	InHeader Location = "header"
	InCookie Location = "cookie"
)

// Style is an OpenAPI parameter style.
type Style string

const (
	StyleSimple         Style = "simple"
	StyleForm           Style = "form"
	StyleLabel          Style = "label"
	StyleMatrix         Style = "matrix"
	StyleSpaceDelimited Style = "space_delimited"
	StylePipeDelimited  Style = "pipe_delimited"
	StyleDeepObject     Style = "deep_object"
)

// Role is what a parameter is for.
type Role string

const (
	RolePlain          Role = "plain"
	RoleIdempotencyKey Role = "idempotency_key"
	RoleDryRun         Role = "dry_run"
	RoleOrigin         Role = "origin"
	RoleAuth           Role = "auth"
	// RoleConstant is a header the runtime sends itself (Constant).
	RoleConstant Role = "constant"
)

// ParamDescriptor describes one parameter.
type ParamDescriptor struct {
	// Name is the key in the arguments object. Roles idempotency_key,
	// origin, auth and constant are not arguments; their name is
	// informational.
	Name     string
	Wire     string
	In       Location
	Required bool
	Style    Style
	Explode  bool
	Role     Role
	// Constant is the header value of a constant parameter, sent on every
	// call; ClientOptions.Headers and CallOptions.Headers replace it.
	Constant  *string
	Sensitive bool
}

// BodyEncoding is how a request body is encoded.
type BodyEncoding string

const (
	EncodeJSON      BodyEncoding = "json"
	EncodeForm      BodyEncoding = "form"
	EncodeMultipart BodyEncoding = "multipart"
	EncodeBytes     BodyEncoding = "bytes"
	EncodeText      BodyEncoding = "text"
)

// MergedField is one field of a merged body: the key in the arguments
// object and the name on the wire.
type MergedField struct {
	Arg  string
	Wire string
}

// BodyDescriptor describes the request body. With Arg set, the whole body
// is args[Arg]; otherwise the body is a JSON object of the merged Fields
// (keys of the arguments object, in wire order).
type BodyDescriptor struct {
	MediaType string
	Encoding  BodyEncoding
	Required  bool
	Fields    []MergedField
	Arg       string
}

// StatusMatch matches a response status.
type StatusMatch struct {
	// Kind is "exact", "class" (1XX to 5XX, Code is the hundreds digit) or
	// "default".
	Kind string
	Code int
}

// Exact matches one status.
func Exact(code int) StatusMatch { return StatusMatch{Kind: "exact", Code: code} }

// Class matches a status class (4 for 4XX).
func Class(digit int) StatusMatch { return StatusMatch{Kind: "class", Code: digit} }

// DefaultStatus matches any status.
var DefaultStatus = StatusMatch{Kind: "default"}

// ResponseKind classifies a declared response.
type ResponseKind string

const (
	KindSuccess   ResponseKind = "success"
	KindError     ResponseKind = "error"
	KindAmbiguous ResponseKind = "ambiguous"
)

// ResponseDescriptor describes one declared response.
type ResponseDescriptor struct {
	Status StatusMatch
	Kind   ResponseKind
	// MediaType is the first media type, "" for a bare status.
	MediaType string
}

// Pagination describes how an operation pages. Style is "cursor",
// "offset", "page" or "link_header"; parameter fields name arguments.
type Pagination struct {
	Style         string
	RequestParam  string
	ResponseField string
	ItemsField    string
	PageSizeParam string
	// HasMoreField: iteration stops when this response boolean is false.
	HasMoreField string
	// CursorItemField: without a cursor in the response, the next cursor is
	// this field of the last item.
	CursorItemField string
	OffsetParam     string
	LimitParam      string
	PageParam       string
	SizeParam       string
}

// IdempotencyKind is an operation's idempotency policy.
type IdempotencyKind string

const (
	IdempotencyNone            IdempotencyKind = "none"
	IdempotencyAuto            IdempotencyKind = "auto"
	IdempotencyCallerOwned     IdempotencyKind = "caller_owned"
	IdempotencyContentHash     IdempotencyKind = "content_hash"
	IdempotencyContentIdentity IdempotencyKind = "content_identity"
)

// RemediationEntry is a manifest entry for an API error code. Empty fields
// are unset.
type RemediationEntry struct {
	Category   Category
	Text       string
	Retryable  Retryable
	NextAction string
}

// VerifyDescriptor is a verification hook: after success, call Operation
// with Args (keys are the read's parameter wire names; "$response." and
// "$args." references resolve against the call) and compare with Expect;
// poll until Terminal holds.
type VerifyDescriptor struct {
	Operation    string
	Args         any
	Expect       any
	Terminal     any
	PollInterval *time.Duration
	PollBudget   *time.Duration
}

// IdempotencyMeta is an operation's idempotency policy.
type IdempotencyMeta struct {
	Policy          IdempotencyKind
	Header          string
	Format          string
	PersistRequired bool
	Note            string
}

// PreviewMode is how preview() works: Mode "local", "header" (Header and
// Value sent as a dry run), "endpoint" (Operation called) or "none".
type PreviewMode struct {
	Mode      string
	Header    string
	Value     string
	Operation string
}

// ConfirmationMeta is the manifest's confirmation text.
type ConfirmationMeta struct {
	SummaryFields []string
	Message       string
}

// AgentMeta is the agent metadata of an operation.
type AgentMeta struct {
	Safety       Safety
	Idempotency  IdempotencyMeta
	Preview      PreviewMode
	Confirmation *ConfirmationMeta
	Verify       *VerifyDescriptor
	// Remediation is operation-specific remediation by API error code.
	Remediation             map[string]RemediationEntry
	RemediationNote         string
	SensitiveResponseFields []string
	// SensitiveRequestFields are dotted argument paths (pin, body.pin).
	SensitiveRequestFields []string
	ShownOnce              bool
}

// Gate is a runtime gate: the operation is mounted only when EnvVar is on;
// otherwise the server answers DisabledStatus.
type Gate struct {
	EnvVar         string
	DisabledStatus int
}

// RPCDescriptor puts an rpc-unflattened operation back on the wire: the
// envelope member Field set to Value, the arguments under ParamsField, and
// Constants sent verbatim.
type RPCDescriptor struct {
	Field       string
	Value       string
	ParamsField string
	Constants   *Object
}

// StreamDescriptor describes the server-sent events of an operation.
type StreamDescriptor struct {
	// Event validates each event's decoded data; nil accepts any JSON.
	Event Validator
	// Done is a data value that ends the stream without being an event.
	Done *string
	// Flag is the wire name of the boolean body field that selects the
	// stream; the stream call sets it to true.
	Flag string
}

// OperationDescriptor is everything the runtime needs to call one
// operation. Generated SDKs are mostly these.
type OperationDescriptor struct {
	ID     string
	Method string
	// Path is the template as in the spec; placeholders use wire names.
	Path      string
	Params    []ParamDescriptor
	Body      *BodyDescriptor
	Responses []ResponseDescriptor
	// Security is an OR of AND-sets of scheme names; empty means no auth.
	Security   [][]string
	Pagination *Pagination
	RPC        *RPCDescriptor
	// ErrorCodeField is the path of the API error code in error bodies.
	ErrorCodeField string
	Gate           *Gate
	Agent          AgentMeta
	// Request validates the arguments object before any network call.
	Request Validator
	// Response validates the success body per ValidateResponses. The
	// result's value is always the decoded wire body.
	Response Validator
	// PageItem validates each item of a page.
	PageItem Validator
	// Summary is the one-line summary; the last line of preview effects.
	Summary string
}

// CompositePart is one part of a composite auth profile: Kind "cookie"
// (Name), "header" (Name, EqualsCookie or FromConfig, MutationOnly) or
// "bearer" (Prefix).
type CompositePart struct {
	Kind         string
	Name         string
	EqualsCookie string
	FromConfig   string
	MutationOnly bool
	Prefix       string
}

// AuthorizationCodeFlow is the authorizationCode flow of an OAuth2 scheme.
// RefreshURL falls back to TokenURL.
type AuthorizationCodeFlow struct {
	AuthorizationURL string
	TokenURL         string
	RefreshURL       string
	Scopes           []string
}

// AuthScheme describes one auth scheme. Kind is "api_key" (In, Wire),
// "http_bearer" (Prefix: the token must start with it), "http_basic",
// "oauth2" (TokenURL, Scopes, AuthorizationCode) or "composite"
// (Satisfies, Parts).
type AuthScheme struct {
	Kind              string
	Name              string
	In                Location
	Wire              string
	Prefix            string
	TokenURL          string
	Scopes            []string
	AuthorizationCode *AuthorizationCodeFlow
	Satisfies         []string
	Parts             []CompositePart
}

// NonJSONError maps an error answer without a JSON body (agent.yml
// errors.non_json). Media is a media type, or "none" for a bare status.
type NonJSONError struct {
	Status    int
	Media     string
	Category  Category
	Retryable Retryable
	Text      string
}

// TierRetries are the retry defaults by tier (agent.yml defaults.retries):
// ReadOnly for reads, Mutating for every other tier (a mutation is still
// retried only with an idempotency key or an identity body).
type TierRetries struct {
	ReadOnly PartialRetry
	Mutating PartialRetry
}

// APIDescriptor describes the API.
type APIDescriptor struct {
	Name            string
	Version         string
	TungstenVersion string
	Servers         []string
	Auth            []AuthScheme
	// ErrorCodes is the global remediation by API error code.
	ErrorCodes map[string]RemediationEntry
	// AmbiguousStatuses make a mutation's outcome unknown.
	AmbiguousStatuses []int
	NonJSON           []NonJSONError
	// Gates maps a gate's environment variable to its explanation.
	Gates map[string]string
	// Retries is nil when the runtime defaults apply to every tier.
	Retries *TierRetries
}

// --------------------------------------------------------------- options

// Credential is the credential of one auth scheme: a secret string for
// api_key, http_bearer and http_basic ("user:password") schemes, or parts
// keyed by cookie name or config key for a composite profile (or the
// client_id, client_secret of an OAuth2 scheme).
type Credential struct {
	secret  string
	parts   map[string]string
	isParts bool
}

// Secret is a secret string credential.
func Secret(value string) Credential { return Credential{secret: value} }

// Parts is a credential made of named parts.
func Parts(parts map[string]string) Credential {
	copied := make(map[string]string, len(parts))
	for k, v := range parts {
		copied[k] = v
	}
	return Credential{parts: copied, isParts: true}
}

// String never shows the credential.
func (c Credential) String() string {
	if c.isParts {
		return fmt.Sprintf("Credential.Parts(%d parts, %s)", len(c.parts), Redacted)
	}
	return "Credential.Secret(" + Redacted + ")"
}

// GoString never shows the credential.
func (c Credential) GoString() string { return c.String() }

// AuthConfig holds the credentials per auth scheme name.
type AuthConfig map[string]Credential

// IdempotencyStore remembers the key used for one logical intent so that a
// retry reuses it.
type IdempotencyStore interface {
	Get(scope, logicalID string) (string, bool)
	Put(scope, logicalID, key string)
}

// RequestContext is what middleware sees of one attempt. Secrets are
// redacted.
type RequestContext struct {
	Operation string
	Attempt   int
	Method    string
	URL       string
	Headers   map[string]string
	// Body is the body as JSON when it is JSON, a text rendering otherwise;
	// HasBody is false without one.
	Body    any
	HasBody bool
}

// ResponseContext is what middleware sees of an answer.
type ResponseContext struct {
	Status  int
	Headers map[string]string
}

// Middleware observes the request lifecycle. Hooks are synchronous and must
// not block.
type Middleware interface {
	OnRequest(ctx *RequestContext)
	OnResponse(ctx *RequestContext, response *ResponseContext)
	OnRetry(ctx *RequestContext, reason *Diagnostic)
}

// MiddlewareFuncs adapts functions to Middleware; nil hooks do nothing.
type MiddlewareFuncs struct {
	Request  func(ctx *RequestContext)
	Response func(ctx *RequestContext, response *ResponseContext)
	Retry    func(ctx *RequestContext, reason *Diagnostic)
}

func (m MiddlewareFuncs) OnRequest(ctx *RequestContext) {
	if m.Request != nil {
		m.Request(ctx)
	}
}

func (m MiddlewareFuncs) OnResponse(ctx *RequestContext, response *ResponseContext) {
	if m.Response != nil {
		m.Response(ctx, response)
	}
}

func (m MiddlewareFuncs) OnRetry(ctx *RequestContext, reason *Diagnostic) {
	if m.Retry != nil {
		m.Retry(ctx, reason)
	}
}

// Jitter is the retry backoff jitter.
type Jitter string

const (
	JitterNone  Jitter = "none"
	JitterFull  Jitter = "full"
	JitterEqual Jitter = "equal"
)

// PartialRetry overrides retry settings field by field; nil fields are
// unset.
type PartialRetry struct {
	// Max is the number of attempts after the first.
	Max             *int
	Base            *time.Duration
	MaxDelay        *time.Duration
	Jitter          Jitter
	HonorRetryAfter *bool
}

// ValidateResponses is how success bodies are checked.
type ValidateResponses string

const (
	// ValidateWarn reports through OnDiagnostic and never fails the call
	// (the default).
	ValidateWarn   ValidateResponses = "warn"
	ValidateOff    ValidateResponses = "off"
	ValidateStrict ValidateResponses = "strict"
)

// Defaults of ClientOptions.
const (
	DefaultTimeout         = 30 * time.Second
	DefaultMaxEventBytes   = 1024 * 1024
	DefaultMaxCollectBytes = 4 * 1024 * 1024
	DefaultMaxCollectTime  = 60 * time.Second
	DefaultMaxReconnects   = 3
	DefaultReconnectMax    = 30 * time.Second
)

// ClientOptions configures a client.
type ClientOptions struct {
	// BaseURL defaults to the first server.
	BaseURL string
	Auth    AuthConfig
	// TokenStore holds the tokens of authorizationCode schemes (configure
	// such a scheme with Parts{"flow": "authorizationCode", "client_id":
	// ...}). Default: in memory, per client.
	TokenStore TokenStore
	// Timeout is the per-attempt timeout (default 30 s).
	Timeout time.Duration
	// Retries overrides the API's per-tier retry defaults, for every tier.
	Retries          PartialRetry
	IdempotencyStore IdempotencyStore
	Middleware       []Middleware
	// ValidateResponses defaults to ValidateWarn.
	ValidateResponses ValidateResponses
	OnDiagnostic      func(d *Diagnostic)
	// Headers are sent on every request.
	Headers map[string]string
	// MaxEventBytes is the largest server-sent event a stream accepts, in
	// UTF-8 bytes (default 1 MiB).
	MaxEventBytes int
	// MaxCollectBytes and MaxCollectTime bound collecting a stream (default
	// 4 MiB and 60 s).
	MaxCollectBytes int
	MaxCollectTime  time.Duration
	// MaxReconnects is how often a stream reconnects after it dropped (a
	// lost connection, or a clean end before the done event the operation
	// declares) with Last-Event-ID set to the last event id it delivered;
	// an event whose id was delivered is never delivered twice. Only for
	// calls that are safe to repeat (reads, or mutations with replay
	// protection) and only once an event id is known (or before the first
	// event). nil means 3; 0 disables it.
	MaxReconnects *int
	// ReconnectMax caps the wait before a reconnect: the server's retry
	// value (250 ms when it sent none), shortened by up to 25 % at random.
	// Default 30 s.
	ReconnectMax time.Duration
	// IdleTimeout: a stream that delivers no bytes (comments count) for this
	// long is idle; it is reconnected like a dropped one, else ends with an
	// error whose code is STREAM_IDLE. Zero: the per-attempt Timeout ends a
	// silent stream, without a reconnect.
	IdleTimeout time.Duration
	// ConfirmationKey signs confirmation tokens; default random per client.
	ConfirmationKey []byte
	// Operations the client resolves by id: verification hooks, endpoint
	// previews and macro steps.
	Operations []*OperationDescriptor
	// HTTPClient is used for every request; it must not follow redirects
	// (the runtime follows same-origin redirects of reads itself). Default:
	// a client without redirects, compression or a default timeout.
	HTTPClient *http.Client
	// Now is the clock in epoch milliseconds.
	Now func() int64
	// Random is a number in [0, 1) for retry jitter.
	Random func() float64
}

// Confirm confirms a destructive or irreversible operation: a token from
// Preview (ConfirmToken) or, for destructive operations only, ConfirmYes.
// The zero value confirms nothing.
type Confirm struct {
	token string
	yes   bool
}

// ConfirmYes accepts a destructive (never an irreversible) operation
// without a token.
var ConfirmYes = Confirm{yes: true}

// ConfirmToken confirms with a token from Preview, bound to the operation
// and the exact arguments.
func ConfirmToken(token string) Confirm { return Confirm{token: token} }

// IsSet reports whether the confirmation carries anything.
func (c Confirm) IsSet() bool { return c.yes || c.token != "" }

// String never shows the token.
func (c Confirm) String() string {
	switch {
	case c.yes:
		return "tungsten.ConfirmYes"
	case c.token != "":
		return "tungsten.ConfirmToken(" + Redacted + ")"
	}
	return "tungsten.Confirm{}"
}

// CallOptions configures one call.
type CallOptions struct {
	// IdempotencyKey is required for caller_owned operations with a
	// required persisted key.
	IdempotencyKey string
	// Confirm: a token authorizes one intent. The first call that sends
	// spends it, and it is accepted again only for a retry with the same
	// idempotency key (or the identical body of a content_identity
	// operation).
	Confirm Confirm
	// RefuseConfirmYes: ConfirmYes does not confirm a destructive operation
	// either, and remediation never offers it (callers that only forward
	// preview tokens, such as a tool server).
	RefuseConfirmYes bool
	// Timeout is the per-attempt timeout; zero uses the client's.
	Timeout time.Duration
	// Verify runs the operation's verification hook after success.
	Verify  bool
	Headers map[string]string
}

// String never shows keys or tokens.
func (o CallOptions) String() string {
	names := make([]string, 0, len(o.Headers))
	for k := range o.Headers {
		names = append(names, k)
	}
	key := ""
	if o.IdempotencyKey != "" {
		key = Redacted
	}
	return fmt.Sprintf("CallOptions{IdempotencyKey: %q, Confirm: %s, Timeout: %s, Verify: %t, Headers: [%s]}",
		key, o.Confirm, o.Timeout, o.Verify, strings.Join(names, " "))
}

// --------------------------------------------------------------- results

// RenderedRequest is a request rendered without sending it; secrets show as
// <redacted>.
type RenderedRequest struct {
	Method  string
	URL     string
	Headers map[string]string
	Body    any
}

// MacroStepPreview is one step of a macro preview.
type MacroStepPreview struct {
	// Step is the 1-based position.
	Step      int
	Kind      string
	Operation string
	As        string
	Safety    Safety
	// Request is nil when the step's arguments (or a bytes or multipart
	// body) come from an earlier result; values from an earlier result show
	// as <from step NAME: path>.
	Request *RenderedRequest
	Effects []string
}

// PreviewResult is what Preview returns.
type PreviewResult struct {
	Operation string
	Safety    Safety
	// Request is the request (for a macro: the first step's).
	Request RenderedRequest
	// Effects summarize the side effects.
	Effects []string
	// ConfirmationToken is set for mutating operations; bound to the exact
	// arguments for ExpiresInMs.
	ConfirmationToken string
	ExpiresInMs       int64
	// ServerPreview is the server's answer for header and endpoint previews.
	ServerPreview    any
	HasServerPreview bool
	// Steps is set for macro previews.
	Steps []MacroStepPreview
}

// Page is one page of a paginated operation.
type Page[T any] struct {
	Items []T
	// Body is the full page body.
	Body any
	// Next is the cursor, offset, page number or URL of the next page; nil
	// at the end.
	Next any
}

// ---------------------------------------------------------------- macros

// MacroStep is one macro step in the canonical form. Args, BudgetMs and the
// macro output are expressions: strings starting with "$" reference $input
// or an earlier step's As name, {"expr": "<ref> in [..]" | "<ref> == x" |
// "<ref> != x"} is a boolean, anything else a literal.
type MacroStep struct {
	// Kind is "call", "poll" or "paginate".
	Kind      string
	Operation string
	Args      any
	As        string
	// Until (poll only) is a predicate on the step's success body.
	Until      any
	IntervalMs *int64
	BudgetMs   any
	// MaxPages (paginate only) is the page limit.
	MaxPages *int64
}

// MacroInput is the macro's input: Extends names the operation whose
// arguments it takes; Add lists added fields (JSON Schema fragments whose
// default applies when the input omits the field), sorted by name.
type MacroInput struct {
	Extends string
	Add     *Object
}

// MacroDescriptor is a compiled macro.
type MacroDescriptor struct {
	Name    string
	Summary string
	Safety  Safety
	Steps   []MacroStep
	Output  any
	Input   MacroInput
	// SensitiveResponseFields are output fields carrying secrets.
	SensitiveResponseFields []string
	// ShownOnce: the output holds values the API returns only once.
	ShownOnce bool
	Cluster   string
}
