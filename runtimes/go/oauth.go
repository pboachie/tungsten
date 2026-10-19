// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"crypto/sha256"
	"fmt"
	"math"
	"sort"
	"strings"
	"sync"
	"time"
)

// ExpirySkewMs: tokens expiring within this many milliseconds count as
// expired.
const ExpirySkewMs = 30000

// StoredToken is what a TokenStore keeps for one scheme. String shows no
// secret.
type StoredToken struct {
	AccessToken  string
	RefreshToken string
	// ExpiresAt is epoch milliseconds; nil when the server gave no lifetime.
	ExpiresAt *int64
	TokenType string
	Scope     string
}

// String never shows the tokens.
func (t StoredToken) String() string {
	return "StoredToken{" + Redacted + "}"
}

// GoString also redacts %#v formatting.
func (t StoredToken) GoString() string { return t.String() }

func cloneStoredToken(t StoredToken) StoredToken {
	if t.ExpiresAt != nil {
		at := *t.ExpiresAt
		t.ExpiresAt = &at
	}
	return t
}

// TokenStore holds the tokens of the authorization-code schemes, by scheme
// name. Implement it to persist tokens.
type TokenStore interface {
	Get(scheme string) (StoredToken, bool)
	Set(scheme string, token StoredToken)
	Delete(scheme string)
}

// FencedTokenStore is required for managed authorization and refresh. Each method
// must invoke current exactly once inside the final serialized mutation, with a
// snapshot of the actual current token. True commits immediately, with no gap,
// retry or speculative predicate call; false preserves the previous token. The
// predicate must not be retained. Return true only after that mutation commits.
// A failed/panicking mutation must preserve the previous token. Implementations
// must not call back into client operations while holding their store lock.
type FencedTokenStore interface {
	TokenStore
	SetIfCurrent(scheme string, token StoredToken, current func(StoredToken, bool) bool) bool
	DeleteIfCurrent(scheme string, current func(StoredToken, bool) bool) bool
}

// MemoryTokenStore is the default store: in memory, per client.
type MemoryTokenStore struct {
	mu     sync.Mutex
	tokens map[string]StoredToken
}

// NewMemoryTokenStore returns an empty store.
func NewMemoryTokenStore() *MemoryTokenStore {
	return &MemoryTokenStore{tokens: map[string]StoredToken{}}
}

// Get implements TokenStore.
func (s *MemoryTokenStore) Get(scheme string) (StoredToken, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	t, ok := s.tokens[scheme]
	return cloneStoredToken(t), ok
}

// Set implements TokenStore.
func (s *MemoryTokenStore) Set(scheme string, token StoredToken) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.tokens == nil {
		s.tokens = map[string]StoredToken{}
	}
	s.tokens[scheme] = cloneStoredToken(token)
}

// Delete implements TokenStore.
func (s *MemoryTokenStore) Delete(scheme string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.tokens, scheme)
}

// SetIfCurrent atomically tests the actual token and conditionally replaces it.
func (s *MemoryTokenStore) SetIfCurrent(scheme string, token StoredToken, current func(StoredToken, bool) bool) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	previous, ok := s.tokens[scheme]
	if !current(cloneStoredToken(previous), ok) {
		return false
	}
	if s.tokens == nil {
		s.tokens = map[string]StoredToken{}
	}
	s.tokens[scheme] = cloneStoredToken(token)
	return true
}

// DeleteIfCurrent atomically tests the actual token and conditionally removes it.
func (s *MemoryTokenStore) DeleteIfCurrent(scheme string, current func(StoredToken, bool) bool) bool {
	s.mu.Lock()
	defer s.mu.Unlock()
	previous, ok := s.tokens[scheme]
	if !current(cloneStoredToken(previous), ok) {
		return false
	}
	delete(s.tokens, scheme)
	return true
}

// PKCE is a PKCE pair (RFC 7636, S256). The verifier is a secret until the
// exchange.
type PKCE struct {
	Verifier  string
	Challenge string
}

// PKCEMethod is always S256.
const PKCEMethod = "S256"

// String shows the challenge only.
func (p PKCE) String() string { return "PKCE{Challenge: " + p.Challenge + "}" }

// GoString never shows the verifier.
func (p PKCE) GoString() string { return p.String() }

// PKCEChallenge is base64url(SHA-256(ASCII(verifier))).
func PKCEChallenge(verifier string) string {
	sum := sha256.Sum256([]byte(verifier))
	return base64URL(sum[:])
}

// GeneratePKCE returns a 43 character verifier from 32 random bytes and its
// challenge.
func GeneratePKCE() (PKCE, bool) {
	b, ok := randomBytes(32)
	if !ok {
		return PKCE{}, false
	}
	verifier := base64URL(b)
	return PKCE{Verifier: verifier, Challenge: PKCEChallenge(verifier)}, true
}

// TokenInfo is what ExchangeCode and Refresh report: no secrets.
type TokenInfo struct {
	Scheme      string
	ExpiresAt   *int64
	Scope       string
	Refreshable bool
}

// LowLevelAuthorizationURLParams are application-owned protocol inputs. This
// escape hatch provides no managed attempt, state/issuer validation or lifetime.
type LowLevelAuthorizationURLParams struct {
	// RedirectURI defaults to the redirect_uri of the scheme's credential.
	RedirectURI string
	// Scopes default to the flow's (nil); an empty non-nil list sends none.
	Scopes        []string
	State         string
	CodeChallenge string
	// Extra query parameters, added in key order; they never override the
	// others.
	Extra map[string]string
}

// LowLevelExchangeCodeParams are application-owned code/verifier inputs.
type LowLevelExchangeCodeParams struct {
	Code         string
	RedirectURI  string
	CodeVerifier string
}

func (p LowLevelAuthorizationURLParams) String() string {
	return "LowLevelAuthorizationURLParams{" + Redacted + "}"
}
func (p LowLevelAuthorizationURLParams) GoString() string { return p.String() }
func (p LowLevelExchangeCodeParams) String() string {
	return "LowLevelExchangeCodeParams{" + Redacted + "}"
}
func (p LowLevelExchangeCodeParams) GoString() string { return p.String() }

func formBody(fields []pair) string {
	parts := make([]string, len(fields))
	for i, f := range fields {
		parts[i] = percentEncode(f.k) + "=" + percentEncode(f.v)
	}
	return strings.Join(parts, "&")
}

func buildAuthorizationURL(flow *AuthorizationCodeFlow, clientID, redirect string, p LowLevelAuthorizationURLParams) string {
	scopes := flow.Scopes
	if p.Scopes != nil {
		scopes = p.Scopes
	}
	scope := strings.Join(scopes, " ")
	fields := []pair{{"response_type", "code"}, {"client_id", clientID}, {"redirect_uri", redirect}}
	if scope != "" {
		fields = append(fields, pair{"scope", scope})
	}
	if p.State != "" {
		fields = append(fields, pair{"state", p.State})
	}
	if p.CodeChallenge != "" {
		fields = append(fields, pair{"code_challenge", p.CodeChallenge}, pair{"code_challenge_method", PKCEMethod})
	}
	reserved := map[string]bool{}
	for _, f := range fields {
		reserved[f.k] = true
	}
	keys := make([]string, 0, len(p.Extra))
	for k := range p.Extra {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		if !reserved[k] {
			fields = append(fields, pair{k, p.Extra[k]})
		}
	}
	base := flow.AuthorizationURL
	sep := "?"
	if strings.Contains(base, "?") {
		sep = "&"
		if strings.HasSuffix(base, "?") || strings.HasSuffix(base, "&") {
			sep = ""
		}
	}
	return base + sep + formBody(fields)
}

type codeClient struct {
	clientID     string
	clientSecret string
	redirectURI  string
}

type tokenFailure struct {
	message  string
	rejected bool
}

func isErrorCode(code string) bool {
	if len(code) < 1 || len(code) > 40 {
		return false
	}
	for i := 0; i < len(code); i++ {
		c := code[i]
		if c != '_' && (c < 'a' || c > 'z') {
			return false
		}
	}
	return true
}

func (c *ClientCore) codeFlow(scheme string) (*AuthorizationCodeFlow, string) {
	for i := range c.api.Auth {
		s := &c.api.Auth[i]
		if s.Name != scheme {
			continue
		}
		if s.Kind == "oauth2" && s.AuthorizationCode != nil {
			return s.AuthorizationCode, ""
		}
		return nil, "the scheme " + scheme + " has no authorizationCode flow"
	}
	return nil, "the API descriptor defines no scheme named " + scheme
}

func (c *ClientCore) codeClient(scheme string) (codeClient, string) {
	invalid := fmt.Sprintf("auth.%s must be tungsten.Parts with flow \"authorizationCode\", client_id, and optionally client_secret and redirect_uri", scheme)
	cred, ok := c.auth[scheme]
	if !ok || !cred.isParts {
		return codeClient{}, invalid
	}
	value := func(snake, camel string) string {
		v, _ := credPart(cred.parts, snake, camel)
		return v
	}
	if value("flow", "flow") != "authorizationCode" {
		return codeClient{}, invalid
	}
	id := value("client_id", "clientId")
	if id == "" {
		return codeClient{}, fmt.Sprintf("auth.%s needs a client_id", scheme)
	}
	return codeClient{clientID: id, clientSecret: value("client_secret", "clientSecret"), redirectURI: value("redirect_uri", "redirectUri")}, ""
}

func oauthError(operation, remediation string) *Error {
	return newDiag(operation, AuthFailed).remedy(remediation).err()
}

func (c *ClientCore) tokenAttemptTimeout() time.Duration {
	if c.timeout < time.Millisecond {
		return time.Millisecond
	}
	return c.timeout
}

func (c *ClientCore) tokenRequest(ctx context.Context, scheme, url string, client codeClient, fields []pair, previous string) (StoredToken, *tokenFailure) {
	if !validOAuthTokenLimit(c.maxOAuthTokenResponseBytes) {
		return StoredToken{}, &tokenFailure{message: "The OAuth token response limit must be a positive safe integer."}
	}
	ctx, cancel := context.WithTimeout(ctx, c.tokenAttemptTimeout())
	defer cancel()
	expired := func() bool {
		deadline, ok := ctx.Deadline()
		return ctx.Err() != nil || (ok && !time.Now().Before(deadline))
	}
	late := &tokenFailure{message: "The OAuth token response was canceled or exceeded its deadline; authorize again."}
	if expired() {
		return StoredToken{}, late
	}
	if insecureHTTP(url, c.allowInsecureHTTP) {
		return StoredToken{}, &tokenFailure{insecureHTTPAuth, false}
	}
	headers := []headerEntry{
		{"Content-Type", "application/x-www-form-urlencoded", false},
		{"Accept", "application/json", false},
	}
	all := append([]pair(nil), fields...)
	if client.clientSecret != "" {
		basic := base64Standard([]byte(percentEncode(client.clientID) + ":" + percentEncode(client.clientSecret)))
		headers = append(headers, headerEntry{"Authorization", "Basic " + basic, true})
	} else {
		all = append(all, pair{"client_id", client.clientID})
	}
	body := payload{bytes: []byte(formBody(all))}
	out := attempt(ctx, c.http, attemptRequest{url: url, method: "POST", headers: headers, body: &body, timeout: c.tokenAttemptTimeout(), maxResponseBytes: &c.maxOAuthTokenResponseBytes})
	// A custom transport/body may ignore cancellation or hold Close. Fence after
	// its actual return, before decoding or treating a 4xx as token rejection.
	if expired() {
		return StoredToken{}, late
	}
	if out.kind != outcomeResponse || !out.bodyOK {
		return StoredToken{}, &tokenFailure{message: fmt.Sprintf("The OAuth2 token endpoint for %s could not be reached; check network access and the token URL.", scheme)}
	}
	decoded := decodeBody(out.body, out.headers, "application/json")
	if expired() {
		return StoredToken{}, late
	}
	if out.status < 200 || out.status > 299 {
		named := ""
		if v, ok := getPathStr(decoded.value, decoded.has, "error"); ok {
			if code, isText := v.(string); isText && isErrorCode(code) {
				named = " (" + code + ")"
			}
		}
		return StoredToken{}, &tokenFailure{
			message:  fmt.Sprintf("The OAuth2 token endpoint for %s answered HTTP %d%s; check the code or refresh token, client credentials and redirect URI.", scheme, out.status, named),
			rejected: out.status >= 400 && out.status < 500,
		}
	}
	stringAt := func(path string) string {
		v, ok := getPathStr(decoded.value, decoded.has, path)
		if !ok {
			return ""
		}
		s, _ := v.(string)
		return s
	}
	access := stringAt("access_token")
	if access == "" {
		return StoredToken{}, &tokenFailure{message: fmt.Sprintf("The OAuth2 token endpoint for %s returned no access_token.", scheme)}
	}
	refresh := stringAt("refresh_token")
	if refresh == "" {
		refresh = previous
	}
	var expires *int64
	if v, ok := getPathStr(decoded.value, decoded.has, "expires_in"); ok {
		if n, isNum := asFloat(v); isNum && !math.IsInf(n, 0) && !math.IsNaN(n) {
			at := c.now() + int64(math.Max(n, 0)*1000)
			expires = &at
		}
	}
	if expired() {
		return StoredToken{}, late
	}
	return StoredToken{AccessToken: access, RefreshToken: refresh, ExpiresAt: expires, TokenType: stringAt("token_type"), Scope: stringAt("scope")}, nil
}

// clientCredentialsToken fetches (and caches) an OAuth2 client-credentials
// token.
func (c *ClientCore) clientCredentialsToken(ctx context.Context, s *satisfier) (string, string) {
	c.ccMu.Lock()
	realNow := time.Now().UnixMilli()
	if cached, ok := c.ccTokens[s.name]; ok && cached.expiresAt > realNow+30000 {
		c.ccMu.Unlock()
		return cached.token, ""
	}
	flight := c.ccFlights[s.name]
	if flight == nil {
		flight = &ccFlight{done: make(chan struct{})}
		c.ccFlights[s.name] = flight
		// The exchange belongs to the shared refresh, not its first waiter.
		// It remains bounded by tokenAttemptTimeout even if every waiter leaves.
		go func() {
			token, reason := c.fetchClientCredentialsToken(context.Background(), s)
			c.ccMu.Lock()
			flight.token, flight.reason = token, reason
			delete(c.ccFlights, s.name)
			close(flight.done)
			c.ccMu.Unlock()
		}()
	}
	c.ccMu.Unlock()
	select {
	case <-ctx.Done():
		return "", "OAuth2 token acquisition was cancelled by the caller's context."
	case <-flight.done:
		return flight.token, flight.reason
	}
}

type ccFlight struct {
	done          chan struct{}
	token, reason string
}

func (c *ClientCore) fetchClientCredentialsToken(ctx context.Context, s *satisfier) (string, string) {
	if insecureHTTP(s.tokenURL, c.allowInsecureHTTP) {
		return "", insecureHTTPAuth
	}
	realNow := time.Now().UnixMilli()
	form := []string{"grant_type=client_credentials"}
	if len(s.scopes) > 0 {
		form = append(form, "scope="+encodeFormComponent(strings.Join(s.scopes, " ")))
	}
	basic := base64Standard([]byte(encodeComponent(s.clientID) + ":" + encodeComponent(s.clientSecret)))
	headers := []headerEntry{
		{"Content-Type", "application/x-www-form-urlencoded", false},
		{"Accept", "application/json", false},
		{"Authorization", "Basic " + basic, true},
	}
	body := payload{bytes: []byte(strings.Join(form, "&"))}
	out := attempt(ctx, c.http, attemptRequest{url: s.tokenURL, method: "POST", headers: headers, body: &body, timeout: c.tokenAttemptTimeout()})
	if out.kind != outcomeResponse || !out.bodyOK {
		return "", fmt.Sprintf("The OAuth2 token endpoint for %s could not be reached; check network access and the token URL.", s.name)
	}
	if out.status < 200 || out.status > 299 {
		return "", fmt.Sprintf("The OAuth2 token endpoint for %s answered HTTP %d; check client_id, client_secret and scopes.", s.name, out.status)
	}
	decoded := decodeBody(out.body, out.headers, "application/json")
	v, ok := getPathStr(decoded.value, decoded.has, "access_token")
	token, _ := v.(string)
	if !ok || token == "" {
		return "", fmt.Sprintf("The OAuth2 token endpoint for %s returned no access_token.", s.name)
	}
	lifetime := int64(3600000)
	if e, ok := getPathStr(decoded.value, decoded.has, "expires_in"); ok {
		if n, isNum := asFloat(e); isNum && !math.IsInf(n, 0) && !math.IsNaN(n) {
			lifetime = int64(math.Max(n*1000, 0))
		}
	}
	c.ccMu.Lock()
	c.ccTokens[s.name] = cachedToken{token: token, expiresAt: realNow + lifetime}
	c.ccMu.Unlock()
	return token, ""
}

type cachedToken struct {
	token     string
	expiresAt int64
}

// OAuthFlow holds the authorization-code helpers of one scheme, as
// generated clients expose them. Failures are AUTH_FAILED envelopes.
type OAuthFlow struct {
	core   *ClientCore
	scheme string
}

// OAuth returns the authorization-code helpers of the scheme.
func (c *ClientCore) OAuth(scheme string) *OAuthFlow {
	return &OAuthFlow{core: c, scheme: scheme}
}

// Scheme is the scheme the helpers belong to.
func (f *OAuthFlow) Scheme() string { return f.scheme }

// TokenStore is the tokens' store.
func (f *OAuthFlow) TokenStore() TokenStore { return f.core.tokenStore }

func (f *OAuthFlow) operation(helper string) string {
	return "oauth." + f.scheme + "." + helper
}

// PKCE returns a low-level pair. Managed AuthorizationURL creates and owns its own pair.
func (f *OAuthFlow) PKCE() (PKCE, error) {
	p, ok := GeneratePKCE()
	if !ok {
		return PKCE{}, oauthError(f.operation("pkce"), "The system has no source of randomness for a PKCE verifier.")
	}
	return p, nil
}

// LowLevelAuthorizationURL builds a URL with application-owned security inputs.
func (f *OAuthFlow) LowLevelAuthorizationURL(p LowLevelAuthorizationURLParams) (string, error) {
	operation := f.operation("authorizationUrl")
	flow, reason := f.core.codeFlow(f.scheme)
	if flow == nil {
		return "", oauthError(operation, "Cannot build an authorization URL: "+reason+".")
	}
	client, reason := f.core.codeClient(f.scheme)
	if reason != "" {
		return "", oauthError(operation, "Cannot build an authorization URL: "+reason+".")
	}
	redirect := p.RedirectURI
	if redirect == "" {
		redirect = client.redirectURI
	}
	if redirect == "" {
		return "", oauthError(operation, fmt.Sprintf("Pass redirect_uri, or set redirect_uri in auth.%s; the authorization URL needs one.", f.scheme))
	}
	return buildAuthorizationURL(flow, client.clientID, redirect, p), nil
}

// LowLevelExchangeCode exchanges application-owned protocol inputs and stores
// tokens without managed callback, generation or atomic-store guarantees.
func (f *OAuthFlow) LowLevelExchangeCode(ctx context.Context, p LowLevelExchangeCodeParams) (TokenInfo, error) {
	operation := f.operation("exchangeCode")
	flow, reason := f.core.codeFlow(f.scheme)
	if flow == nil {
		return TokenInfo{}, oauthError(operation, "Cannot exchange the code: "+reason+".")
	}
	client, reason := f.core.codeClient(f.scheme)
	if reason != "" {
		return TokenInfo{}, oauthError(operation, "Cannot exchange the code: "+reason+".")
	}
	if p.Code == "" {
		return TokenInfo{}, oauthError(operation, "Pass the authorization code the server redirected back with as code.")
	}
	redirect := p.RedirectURI
	if redirect == "" {
		redirect = client.redirectURI
	}
	if redirect == "" {
		return TokenInfo{}, oauthError(operation, fmt.Sprintf("Pass redirect_uri, or set redirect_uri in auth.%s; it must equal the one used for the authorization URL.", f.scheme))
	}
	fields := []pair{{"grant_type", "authorization_code"}, {"code", p.Code}, {"redirect_uri", redirect}}
	if p.CodeVerifier != "" {
		fields = append(fields, pair{"code_verifier", p.CodeVerifier})
	}
	stored, failure := f.core.tokenRequest(ctx, f.scheme, flow.TokenURL, client, fields, "")
	if failure != nil {
		return TokenInfo{}, oauthError(operation, failure.message)
	}
	f.core.tokenStore.Set(f.scheme, stored)
	return tokenInfo(f.scheme, stored), nil
}

func tokenInfo(scheme string, t StoredToken) TokenInfo {
	t = cloneStoredToken(t)
	return TokenInfo{Scheme: scheme, ExpiresAt: t.ExpiresAt, Scope: t.Scope, Refreshable: t.RefreshToken != ""}
}
