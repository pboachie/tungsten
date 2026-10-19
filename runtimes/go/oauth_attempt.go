// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"crypto/subtle"
	"net/url"
	"strings"
	"sync"
	"time"
	"unicode/utf8"
)

const (
	maxOAuthAttempts                     = 32
	maxOAuthItems                        = 64
	maxOAuthCaptureBytes                 = 32768
	maxOAuthCallbackBytes                = 16384
	defaultOAuthTokenResponseBytes int64 = 65536
	oauthAttemptLifetime                 = 10 * time.Minute
)

// IssuerDefense pins a provider-qualified mix-up defense; missing iss never
// activates an alternate strategy. Its zero value refuses authorization.
type IssuerDefense struct {
	response bool
	value    string
}

// IssuerResponse requires this exact decoded iss on success and denial.
func IssuerResponse(issuer string) IssuerDefense { return IssuerDefense{true, issuer} }

// DistinctRedirect records the application's qualification of a redirect that
// isolates this issuer from other issuers. The runtime cannot establish that fact.
func DistinctRedirect(qualification string) IssuerDefense { return IssuerDefense{false, qualification} }
func (d IssuerDefense) String() string                    { return "IssuerDefense{" + Redacted + "}" }
func (d IssuerDefense) GoString() string                  { return d.String() }

// AuthorizationURLParams begin a managed attempt. Bind the authenticated browser
// session or owning native listener; qualify provider S256 and issuer defense.
type AuthorizationURLParams struct {
	SessionBinding string
	S256Supported  bool
	IssuerDefense  IssuerDefense
	RedirectURI    string
	Scopes         []string
	Resource       string
	Audience       string
	AccountContext string
	Extra          map[string]string
}

func (p AuthorizationURLParams) String() string   { return "AuthorizationURLParams{" + Redacted + "}" }
func (p AuthorizationURLParams) GoString() string { return p.String() }

// AuthorizationAttempt carries a URL and opaque handle for the owning listener.
// Neither is included in formatting. Do not log its fields or serialize it.
type AuthorizationAttempt struct {
	AttemptHandle string
	URL           string
	ExpiresInMs   int64
}

func (p AuthorizationAttempt) String() string   { return "AuthorizationAttempt{" + Redacted + "}" }
func (p AuthorizationAttempt) GoString() string { return p.String() }

// ExchangeCodeParams complete one attempt using the entire callback URL and
// its owning binding. Codes/verifiers are accepted only by LowLevelExchangeCode.
type ExchangeCodeParams struct {
	AttemptHandle  string
	CallbackURL    string
	SessionBinding string
}

func (p ExchangeCodeParams) String() string   { return "ExchangeCodeParams{" + Redacted + "}" }
func (p ExchangeCodeParams) GoString() string { return p.String() }

func validOAuthTokenLimit(value int64) bool { return value > 0 && value <= 9007199254740991 }
func oauthText(value string, maximum int) bool {
	return len(value) <= maximum && utf8.ValidString(value)
}
func oauthReserved(key string) bool {
	switch key {
	case "response_type", "client_id", "client_secret", "redirect_uri", "scope", "resource", "audience", "state", "code_challenge", "code_challenge_method", "code_verifier", "grant_type", "code", "iss", "error", "error_description", "error_uri", "request", "request_uri", "response_mode":
		return true
	}
	return false
}

// Count before splitting/decoding; reject duplicate decoded keys and malformed
// percent/UTF-8 input before query values can be folded into a map.
func oauthFields(query string) (map[string]string, bool) {
	if !oauthText(query, maxOAuthCallbackBytes) {
		return nil, false
	}
	count := 0
	if query != "" {
		count = 1
	}
	for i := 0; i < len(query); i++ {
		if query[i] == '&' {
			count++
		}
		if count > maxOAuthItems {
			return nil, false
		}
	}
	fields := make(map[string]string, count)
	if query == "" {
		return fields, true
	}
	size := 0
	for _, part := range strings.Split(query, "&") {
		key, value, ok := strings.Cut(part, "=")
		if !ok || key == "" {
			return nil, false
		}
		key, err := url.QueryUnescape(key)
		if err != nil || key == "" || !utf8.ValidString(key) {
			return nil, false
		}
		value, err = url.QueryUnescape(value)
		if err != nil || !utf8.ValidString(value) {
			return nil, false
		}
		size += len(key) + len(value)
		if _, exists := fields[key]; exists || size > maxOAuthCallbackBytes {
			return nil, false
		}
		fields[key] = value
	}
	return fields, true
}

func qualifiedOAuthEndpoint(value string, redirect bool) bool {
	// This check must precede URL parsing, including for mutated/preloaded refresh.
	if !oauthText(value, maxOAuthCaptureBytes) || strings.Contains(value, "#") {
		return false
	}
	u, err := url.Parse(value)
	if err != nil || !u.IsAbs() || u.User != nil {
		return false
	}
	if (u.Scheme == "http" || u.Scheme == "https") && u.Hostname() == "" {
		return false
	}
	query := strings.IndexByte(value, '?')
	if redirect {
		return query < 0
	}
	if (u.Scheme != "http" && u.Scheme != "https") || u.Hostname() == "" {
		return false
	}
	if query >= 0 {
		fields, ok := oauthFields(value[query+1:])
		if !ok {
			return false
		}
		for key := range fields {
			if oauthReserved(key) {
				return false
			}
		}
	}
	return true
}

type oauthBudget struct{ bytes int }

func (b *oauthBudget) charge(value string) bool {
	if !oauthText(value, maxOAuthCaptureBytes-b.bytes) {
		return false
	}
	b.bytes += len(value)
	return true
}
func (b *oauthBudget) scopes(values []string) ([]string, bool) {
	if len(values) > maxOAuthItems {
		return nil, false
	}
	copy := make([]string, 0, len(values))
	for _, value := range values {
		if !b.charge(value) {
			return nil, false
		}
		copy = append(copy, value)
	}
	return copy, true
}

type oauthVersion struct{ generation, epoch uint64 }
type oauthSession struct {
	generation, epoch uint64
	loggingOut        int
	closed            bool
}
type oauthState struct {
	mu        sync.Mutex
	sessions  map[string]*oauthSession
	attempts  map[string]*pendingAuthorization
	beginning int
	refreshes map[oauthRefreshKey]*oauthRefresh
	logouts   int
}
type pendingAuthorization struct {
	scheme, handle, binding, state, verifier, redirect, link string
	client                                                   codeClient
	flow                                                     AuthorizationCodeFlow
	defense                                                  IssuerDefense
	requestedScopes                                          []string
	resource, audience, accountContext                       string
	generation                                               uint64
	deadline                                                 time.Time
	claimed, canceled                                        bool
	timer                                                    *time.Timer
	stopBegin                                                chan struct{}
	beginDone                                                <-chan struct{}
	cancel                                                   context.CancelFunc
}

// All methods ending Locked require oauth.mu. No token store or application
// callback may be called with that lock held. Store predicates acquire this lock
// only from inside the store's final serialized conditional mutation.
func (c *ClientCore) oauthSessionLocked(scheme string) *oauthSession {
	if c.oauth.sessions == nil {
		c.oauth.sessions = map[string]*oauthSession{}
	}
	s := c.oauth.sessions[scheme]
	if s == nil {
		s = &oauthSession{}
		c.oauth.sessions[scheme] = s
	}
	return s
}
func (c *ClientCore) oauthActiveLocked(scheme string, generation uint64) bool {
	s := c.oauthSessionLocked(scheme)
	return !s.closed && s.loggingOut == 0 && s.generation == generation
}
func (c *ClientCore) oauthCurrentLocked(scheme string, version oauthVersion) bool {
	return c.oauthActiveLocked(scheme, version.generation) && c.oauthSessionLocked(scheme).epoch == version.epoch
}
func (c *ClientCore) oauthMutateLocked(scheme string) (uint64, bool) {
	s := c.oauthSessionLocked(scheme)
	if s.epoch == ^uint64(0) {
		s.closed = true
		return 0, false
	}
	s.epoch++
	return s.epoch, true
}
func (c *ClientCore) oauthCurrent(scheme string, version oauthVersion) bool {
	c.oauth.mu.Lock()
	defer c.oauth.mu.Unlock()
	return c.oauthCurrentLocked(scheme, version)
}
func (c *ClientCore) authorizationLiveLocked(p *pendingAuthorization) bool {
	select {
	case <-p.beginDone:
		return false
	default:
	}
	return !p.canceled && time.Now().Before(p.deadline) && c.oauthActiveLocked(p.scheme, p.generation)
}
func (c *ClientCore) invalidateAuthorizationLocked(p *pendingAuthorization) {
	p.canceled = true
	p.state, p.verifier = "", ""
	if p.timer != nil {
		p.timer.Stop()
		p.timer = nil
	}
	if p.stopBegin != nil {
		close(p.stopBegin)
		p.stopBegin = nil
	}
	if p.cancel != nil {
		p.cancel()
	}
	if !p.claimed && c.oauth.attempts[p.handle] == p {
		delete(c.oauth.attempts, p.handle)
	}
}
func (c *ClientCore) endAuthorization(p *pendingAuthorization) bool {
	c.oauth.mu.Lock()
	defer c.oauth.mu.Unlock()
	live := c.authorizationLiveLocked(p)
	p.cancel = nil // normal retirement must publish its result before canceling.
	c.invalidateAuthorizationLocked(p)
	if c.oauth.attempts[p.handle] == p {
		delete(c.oauth.attempts, p.handle)
	}
	return live
}

func (c *ClientCore) oauthConfigurationMatches(p *pendingAuthorization) bool {
	flow, reason := c.codeFlow(p.scheme)
	if reason != "" || flow == nil || flow.AuthorizationURL != p.flow.AuthorizationURL || flow.TokenURL != p.flow.TokenURL || flow.RefreshURL != p.flow.RefreshURL || len(flow.Scopes) != len(p.flow.Scopes) {
		return false
	}
	for i, value := range p.flow.Scopes {
		if flow.Scopes[i] != value {
			return false
		}
	}
	client, reason := c.codeClient(p.scheme)
	return reason == "" && client == p.client
}

func (c *ClientCore) captureAuthorization(scheme string, in AuthorizationURLParams) (*pendingAuthorization, bool) {
	flow, reason := c.codeFlow(scheme)
	client, why := c.codeClient(scheme)
	if flow == nil || reason != "" || why != "" || !in.S256Supported || in.SessionBinding == "" || in.IssuerDefense.value == "" {
		return nil, false
	}
	redirect := in.RedirectURI
	if redirect == "" {
		redirect = client.redirectURI
	}
	b := oauthBudget{}
	for _, value := range []string{scheme, flow.AuthorizationURL, flow.TokenURL, flow.RefreshURL, client.clientID, client.clientSecret, client.redirectURI, redirect, in.SessionBinding, in.IssuerDefense.value, in.Resource, in.Audience, in.AccountContext} {
		if !b.charge(value) {
			return nil, false
		}
	}
	flowScopes, ok := b.scopes(flow.Scopes)
	if !ok {
		return nil, false
	}
	scopes := flowScopes
	if in.Scopes != nil {
		scopes, ok = b.scopes(in.Scopes)
	}
	if !ok || len(in.Extra) > maxOAuthItems {
		return nil, false
	}
	extra := make(map[string]string, len(in.Extra)+2)
	for key, value := range in.Extra {
		if key == "" || oauthReserved(key) || !b.charge(key) || !b.charge(value) {
			return nil, false
		}
		extra[key] = value
	}
	if !qualifiedOAuthEndpoint(flow.AuthorizationURL, false) || !qualifiedOAuthEndpoint(flow.TokenURL, false) || !qualifiedOAuthEndpoint(redirect, true) || insecureHTTP(flow.AuthorizationURL, c.allowInsecureHTTP) || insecureHTTP(flow.TokenURL, c.allowInsecureHTTP) {
		return nil, false
	}
	if flow.RefreshURL != "" && (!qualifiedOAuthEndpoint(flow.RefreshURL, false) || insecureHTTP(flow.RefreshURL, c.allowInsecureHTTP)) {
		return nil, false
	}
	pkce, ok := GeneratePKCE()
	if !ok {
		return nil, false
	}
	state, ok := randomBytes(32)
	if !ok {
		return nil, false
	}
	handle, ok := randomBytes(32)
	if !ok {
		return nil, false
	}
	owned := AuthorizationCodeFlow{AuthorizationURL: flow.AuthorizationURL, TokenURL: flow.TokenURL, RefreshURL: flow.RefreshURL, Scopes: flowScopes}
	if in.Resource != "" {
		extra["resource"] = in.Resource
	}
	if in.Audience != "" {
		extra["audience"] = in.Audience
	}
	p := &pendingAuthorization{scheme: scheme, handle: base64URL(handle), binding: in.SessionBinding, state: base64URL(state), verifier: pkce.Verifier, redirect: redirect, client: client, flow: owned, defense: in.IssuerDefense, requestedScopes: scopes, resource: in.Resource, audience: in.Audience, accountContext: in.AccountContext, deadline: time.Now().Add(oauthAttemptLifetime)}
	p.link = buildAuthorizationURL(&owned, client.clientID, redirect, LowLevelAuthorizationURLParams{Scopes: scopes, State: p.state, CodeChallenge: pkce.Challenge, Extra: extra})
	return p, true
}

// AuthorizationURL begins a managed attempt. Its context owns the attempt until
// completion/cancel/expiry; do not cancel it merely because the browser is opened.
func (f *OAuthFlow) AuthorizationURL(ctx context.Context, in AuthorizationURLParams) (AuthorizationAttempt, error) {
	c := f.core
	refuse := func() (AuthorizationAttempt, error) {
		return AuthorizationAttempt{}, oauthError(f.operation("authorizationUrl"), "Authorization requires bounded valid inputs, provider-qualified S256 and issuer defense, an owning session/listener binding, and an atomic fenced token store.")
	}
	if _, ok := c.tokenStore.(FencedTokenStore); !ok || !validOAuthTokenLimit(c.maxOAuthTokenResponseBytes) || !oauthText(f.scheme, maxOAuthCaptureBytes) || ctx.Err() != nil {
		return refuse()
	}
	if flow, _ := c.codeFlow(f.scheme); flow == nil {
		return refuse()
	}
	// Reserve before randomness/capture. No eviction of a claimed or beginning owner.
	c.oauth.mu.Lock()
	s := c.oauthSessionLocked(f.scheme)
	generation := s.generation
	if !c.oauthActiveLocked(f.scheme, generation) || len(c.oauth.attempts)+c.oauth.beginning >= maxOAuthAttempts {
		c.oauth.mu.Unlock()
		return refuse()
	}
	c.oauth.beginning++
	c.oauth.mu.Unlock()
	p, ok := c.captureAuthorization(f.scheme, in)
	canceled := ctx.Err() != nil
	beginDone := ctx.Done() // never invoke a custom Context method under oauth.mu.
	c.oauth.mu.Lock()
	c.oauth.beginning--
	if !ok || canceled || !c.oauthActiveLocked(f.scheme, generation) {
		c.oauth.mu.Unlock()
		return refuse()
	}
	p.generation = generation
	p.beginDone = beginDone
	if c.oauth.attempts == nil {
		c.oauth.attempts = map[string]*pendingAuthorization{}
	}
	if _, exists := c.oauth.attempts[p.handle]; exists {
		c.oauth.mu.Unlock()
		return refuse()
	}
	c.oauth.attempts[p.handle] = p
	c.oauth.mu.Unlock()
	cancelAttempt := func() { c.oauth.mu.Lock(); defer c.oauth.mu.Unlock(); c.invalidateAuthorizationLocked(p) }
	timer := time.AfterFunc(time.Until(p.deadline), cancelAttempt)
	stop := make(chan struct{})
	c.oauth.mu.Lock()
	live := c.authorizationLiveLocked(p) && c.oauth.attempts[p.handle] == p
	if live {
		p.timer, p.stopBegin = timer, stop
	} else {
		timer.Stop()
		close(stop)
	}
	c.oauth.mu.Unlock()
	// This watcher performs only channel operations and internal cancellation;
	// it cannot retain arbitrary application work after the attempt is retired.
	go func() {
		select {
		case <-beginDone:
			cancelAttempt()
		case <-stop:
		}
	}()
	if !live || ctx.Err() != nil {
		cancelAttempt()
		return refuse()
	}
	return AuthorizationAttempt{AttemptHandle: p.handle, URL: p.link, ExpiresInMs: oauthAttemptLifetime.Milliseconds()}, nil
}

func oauthEqual(a, b string) bool { return subtle.ConstantTimeCompare([]byte(a), []byte(b)) == 1 }
func callbackCode(p *pendingAuthorization, in ExchangeCodeParams) (string, bool, bool) {
	if !oauthText(in.CallbackURL, maxOAuthCallbackBytes) || strings.Contains(in.CallbackURL, "#") {
		return "", false, false
	}
	redirect, query, ok := strings.Cut(in.CallbackURL, "?")
	if !ok || redirect != p.redirect {
		return "", false, false
	}
	fields, ok := oauthFields(query)
	if !ok || !oauthEqual(fields["state"], p.state) || (p.defense.response && fields["iss"] != p.defense.value) {
		return "", false, false
	}
	code, hasCode := fields["code"]
	denial, denied := fields["error"]
	if hasCode == denied || (hasCode && code == "") || (denied && denial == "") {
		return "", false, false
	}
	if hasCode {
		if _, ok := fields["error_description"]; ok {
			return "", false, false
		}
		if _, ok := fields["error_uri"]; ok {
			return "", false, false
		}
	}
	return code, denied, true
}

// ExchangeCode atomically claims a bound, valid full callback exactly once. A
// canceled waiter cannot reopen a code or retire its physically unsettled owner.
func (f *OAuthFlow) ExchangeCode(ctx context.Context, in ExchangeCodeParams) (TokenInfo, error) {
	c := f.core
	refuse := func() (TokenInfo, error) {
		return TokenInfo{}, oauthError(f.operation("exchangeCode"), "Authorization callback refused: invalid, expired, consumed or mismatched attempt. Begin a new authorization attempt if needed.")
	}
	if !oauthText(in.AttemptHandle, maxOAuthCaptureBytes) || !oauthText(in.SessionBinding, maxOAuthCaptureBytes) || ctx.Err() != nil {
		return refuse()
	}
	callerDone := ctx.Done()
	started := time.Now()
	deadline := started.Add(c.tokenAttemptTimeout())
	if caller, ok := ctx.Deadline(); ok {
		// Parsed/rounded caller Times can lack monotonic data. Interpret that
		// bound once against the captured start, preserving our earlier budget
		// and never rebasing it after a context callback or other work.
		ownedCaller := started.Add(caller.Sub(started))
		if ownedCaller.Before(deadline) {
			deadline = ownedCaller
		}
	}
	// Cancellation is bridged outside oauth.mu; the owned context has no custom
	// parent cancellation callbacks to execute while a store predicate holds it.
	workCtx, cancel := context.WithDeadline(context.WithoutCancel(ctx), deadline)
	c.oauth.mu.Lock()
	p := c.oauth.attempts[in.AttemptHandle]
	if p == nil || p.scheme != f.scheme || !c.authorizationLiveLocked(p) || p.claimed || !oauthEqual(in.SessionBinding, p.binding) || workCtx.Err() != nil {
		c.oauth.mu.Unlock()
		cancel()
		return refuse()
	}
	code, denied, ok := callbackCode(p, in)
	if !ok || !c.oauthConfigurationMatches(p) {
		c.oauth.mu.Unlock()
		cancel()
		return refuse()
	}
	if denied {
		c.invalidateAuthorizationLocked(p)
		c.oauth.mu.Unlock()
		cancel()
		return TokenInfo{}, oauthError(f.operation("exchangeCode"), "Authorization was denied. Begin a new authorization attempt to authorize again.")
	}
	if p.deadline.Before(deadline) {
		cancel()
		deadline = p.deadline
		workCtx, cancel = context.WithDeadline(context.WithoutCancel(ctx), deadline)
	}
	p.claimed, p.cancel = true, cancel
	verifier := p.verifier
	p.verifier = ""
	c.oauth.mu.Unlock()
	stopCaller := context.AfterFunc(ctx, cancel)
	defer stopCaller()
	workLive := func() bool {
		select {
		case <-callerDone:
			return false
		default:
		}
		return oauthBeforeDeadline(workCtx)
	}
	type completion struct {
		token   TokenInfo
		version oauthVersion
		err     error
	}
	done := make(chan completion, 1)
	go func() {
		result := completion{err: oauthError(f.operation("exchangeCode"), "Authorization exchange did not complete. The code may have been consumed; begin a new authorization attempt.")}
		defer func() {
			if recover() != nil {
				result = completion{err: oauthError(f.operation("exchangeCode"), "Authorization exchange or storage did not complete; begin a new attempt.")}
			}
			live := c.endAuthorization(p)
			if result.err == nil && (!live || !workLive()) {
				result = completion{err: oauthError(f.operation("exchangeCode"), "Authorization was canceled or expired; begin a new attempt.")}
			}
			done <- result
			cancel()
		}()
		if !workLive() || !c.oauthConfigurationMatches(p) {
			return
		}
		fields := []pair{{"grant_type", "authorization_code"}, {"code", code}, {"redirect_uri", p.redirect}, {"code_verifier", verifier}}
		stored, failure := c.tokenRequest(workCtx, f.scheme, p.flow.TokenURL, p.client, fields, "")
		verifier = ""
		if failure != nil || !workLive() {
			return
		}
		var committed oauthVersion
		changed := c.tokenStore.(FencedTokenStore).SetIfCurrent(f.scheme, stored, func(_ StoredToken, _ bool) bool {
			c.oauth.mu.Lock()
			defer c.oauth.mu.Unlock()
			if !workLive() || !c.authorizationLiveLocked(p) || !c.oauthConfigurationMatches(p) {
				return false
			}
			epoch, ok := c.oauthMutateLocked(f.scheme)
			committed = oauthVersion{p.generation, epoch}
			return ok
		})
		if !changed || !workLive() || !c.oauthCurrent(f.scheme, committed) {
			return
		}
		result = completion{token: tokenInfo(f.scheme, stored), version: committed}
	}()
	finish := func(result completion) (TokenInfo, error) {
		if ctx.Err() != nil || !time.Now().Before(deadline) || (result.err == nil && !c.oauthCurrent(f.scheme, result.version)) {
			return refuse()
		}
		return result.token, result.err
	}
	select {
	case <-callerDone:
		c.oauth.mu.Lock()
		c.invalidateAuthorizationLocked(p)
		c.oauth.mu.Unlock()
		return refuse()
	case <-workCtx.Done():
		select {
		case result := <-done:
			return finish(result)
		default:
		}
		c.oauth.mu.Lock()
		c.invalidateAuthorizationLocked(p)
		c.oauth.mu.Unlock()
		return refuse()
	case result := <-done:
		return finish(result)
	}
}

// CancelAuthorization invalidates only a handle owned by this session/listener.
// A claimed owner's slot remains occupied until token/store work really returns.
func (f *OAuthFlow) CancelAuthorization(handle, binding string) error {
	c := f.core
	if !oauthText(handle, maxOAuthCaptureBytes) || !oauthText(binding, maxOAuthCaptureBytes) {
		return oauthError(f.operation("cancelAuthorization"), "Authorization attempt is unavailable or belongs to a different session.")
	}
	c.oauth.mu.Lock()
	defer c.oauth.mu.Unlock()
	p := c.oauth.attempts[handle]
	if p == nil || p.scheme != f.scheme || !oauthEqual(p.binding, binding) {
		return oauthError(f.operation("cancelAuthorization"), "Authorization attempt is unavailable or belongs to a different session.")
	}
	c.invalidateAuthorizationLocked(p)
	return nil
}

// Logout invalidates this scheme's generation before removing its token. An
// uncooperative store keeps admission and blocks new managed logins until its
// removal returns, even when the caller stops waiting. At most 32 removals run.
func (f *OAuthFlow) Logout(ctx context.Context) error {
	c := f.core
	failure := func() error {
		return oauthError(f.operation("logout"), "Local credential removal did not complete; check the token store before authorizing again.")
	}
	if ctx.Err() != nil {
		return failure()
	}
	if flow, _ := c.codeFlow(f.scheme); flow == nil {
		return failure()
	}
	workCtx, cancel := context.WithTimeout(context.Background(), c.tokenAttemptTimeout())
	stopCaller := context.AfterFunc(ctx, cancel)
	defer stopCaller()
	c.oauth.mu.Lock()
	s := c.oauthSessionLocked(f.scheme)
	if c.oauth.logouts >= maxOAuthAttempts || s.generation == ^uint64(0) {
		if s.generation == ^uint64(0) {
			s.closed = true
		}
		c.oauth.mu.Unlock()
		cancel()
		return failure()
	}
	s.generation++
	s.loggingOut++
	c.oauth.logouts++
	for _, p := range c.oauth.attempts {
		if p.scheme == f.scheme {
			c.invalidateAuthorizationLocked(p)
		}
	}
	for key, flight := range c.oauth.refreshes {
		if key.scheme == f.scheme {
			flight.cancel()
		}
	}
	c.oauth.mu.Unlock()
	done := make(chan bool, 1)
	deadline, _ := workCtx.Deadline()
	go func() {
		ok := false
		defer func() {
			if recover() != nil {
				ok = false
			}
			c.oauth.mu.Lock()
			s.loggingOut--
			c.oauth.logouts--
			c.oauth.mu.Unlock()
			done <- ok
			cancel()
		}()
		c.tokenStore.Delete(f.scheme)
		ok = true
	}()
	finish := func(ok bool) error {
		if ok && ctx.Err() == nil && time.Now().Before(deadline) {
			return nil
		}
		return failure()
	}
	select {
	case ok := <-done:
		return finish(ok)
	case <-workCtx.Done():
		select {
		case ok := <-done:
			return finish(ok)
		default:
			return failure()
		}
	}
}
