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

// TokenStore holds the tokens of the authorization-code schemes, by scheme
// name. Implement it to persist tokens.
type TokenStore interface {
	Get(scheme string) (StoredToken, bool)
	Set(scheme string, token StoredToken)
	Delete(scheme string)
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
	return t, ok
}

// Set implements TokenStore.
func (s *MemoryTokenStore) Set(scheme string, token StoredToken) {
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.tokens == nil {
		s.tokens = map[string]StoredToken{}
	}
	s.tokens[scheme] = token
}

// Delete implements TokenStore.
func (s *MemoryTokenStore) Delete(scheme string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	delete(s.tokens, scheme)
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

// AuthorizationURLParams are the parameters of AuthorizationURL.
type AuthorizationURLParams struct {
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

// ExchangeCodeParams are the parameters of ExchangeCode.
type ExchangeCodeParams struct {
	Code         string
	RedirectURI  string
	CodeVerifier string
}

func formBody(fields []pair) string {
	parts := make([]string, len(fields))
	for i, f := range fields {
		parts[i] = percentEncode(f.k) + "=" + percentEncode(f.v)
	}
	return strings.Join(parts, "&")
}

func buildAuthorizationURL(flow *AuthorizationCodeFlow, clientID, redirect string, p AuthorizationURLParams) string {
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

// gate serializes the refreshes of one scheme; generation counts completed
// ones so a waiter can use the token another caller refreshed.
type gate struct {
	mu         sync.Mutex
	generation uint64
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

func (c *ClientCore) gateOf(scheme string) *gate {
	c.mu.Lock()
	defer c.mu.Unlock()
	g, ok := c.oauthGates[scheme]
	if !ok {
		g = &gate{}
		c.oauthGates[scheme] = g
	}
	return g
}

func (c *ClientCore) readToken(scheme string) (StoredToken, bool) {
	t, ok := c.tokenStore.Get(scheme)
	if !ok || t.AccessToken == "" {
		return StoredToken{}, false
	}
	return t, true
}

// storedToken is the access token to send: the stored one, refreshed first
// when it expires within ExpirySkewMs (or force). The problem is the text of
// the AUTH_FAILED envelope.
func (c *ClientCore) storedToken(ctx context.Context, scheme string, force bool) (string, string) {
	token, failure := c.storedTokenInner(ctx, scheme, force)
	if failure != nil {
		return "", failure.message
	}
	return token, ""
}

func (c *ClientCore) storedTokenInner(ctx context.Context, scheme string, force bool) (string, *tokenFailure) {
	missing := &tokenFailure{message: fmt.Sprintf("No OAuth2 token is stored for %s; send the user to authorization_url(), then call exchange_code() with the code, or put a token in ClientOptions.token_store.", scheme)}
	stored, ok := c.readToken(scheme)
	if !ok {
		return "", missing
	}
	fresh := stored.ExpiresAt == nil || *stored.ExpiresAt > c.now()+ExpirySkewMs
	if fresh && !force {
		return stored.AccessToken, nil
	}
	g := c.gateOf(scheme)
	c.mu.Lock()
	seen := g.generation
	c.mu.Unlock()
	g.mu.Lock()
	defer g.mu.Unlock()
	c.mu.Lock()
	changed := g.generation != seen
	c.mu.Unlock()
	if changed {
		t, ok := c.readToken(scheme)
		if !ok {
			return "", missing
		}
		return t.AccessToken, nil
	}
	current, ok := c.readToken(scheme)
	if !ok {
		return "", missing
	}
	token, failure := c.refreshStored(ctx, scheme, current, force)
	if failure != nil {
		return "", failure
	}
	c.mu.Lock()
	g.generation++
	c.mu.Unlock()
	return token, nil
}

// refreshAfterRejection forces a refresh after a 401 when the stored token
// has a refresh token.
func (c *ClientCore) refreshAfterRejection(ctx context.Context, scheme, rejected string) (string, bool) {
	stored, ok := c.readToken(scheme)
	if !ok {
		return "", false
	}
	if stored.AccessToken != rejected {
		return stored.AccessToken, true
	}
	if stored.RefreshToken == "" {
		return "", false
	}
	token, problem := c.storedToken(ctx, scheme, true)
	return token, problem == ""
}

func (c *ClientCore) refreshStored(ctx context.Context, scheme string, stored StoredToken, force bool) (string, *tokenFailure) {
	flow, reason := c.codeFlow(scheme)
	if flow == nil {
		return "", &tokenFailure{message: fmt.Sprintf("Cannot refresh the OAuth2 token for %s: %s.", scheme, reason)}
	}
	client, reason := c.codeClient(scheme)
	if reason != "" {
		return "", &tokenFailure{message: fmt.Sprintf("Cannot refresh the OAuth2 token for %s: %s.", scheme, reason)}
	}
	if stored.RefreshToken == "" {
		why := "expired and has no refresh token"
		if force {
			why = "has no refresh token"
		}
		return "", &tokenFailure{message: fmt.Sprintf("The OAuth2 token for %s %s; authorize again with authorization_url() and exchange_code().", scheme, why)}
	}
	url := flow.RefreshURL
	if url == "" {
		url = flow.TokenURL
	}
	fields := []pair{{"grant_type", "refresh_token"}, {"refresh_token", stored.RefreshToken}}
	renewed, failure := c.tokenRequest(ctx, scheme, url, client, fields, stored.RefreshToken)
	if failure != nil {
		if failure.rejected {
			c.tokenStore.Delete(scheme)
		}
		return "", failure
	}
	c.tokenStore.Set(scheme, renewed)
	return renewed.AccessToken, nil
}

func (c *ClientCore) tokenAttemptTimeout() time.Duration {
	if c.timeout < time.Millisecond {
		return time.Millisecond
	}
	return c.timeout
}

func (c *ClientCore) tokenRequest(ctx context.Context, scheme, url string, client codeClient, fields []pair, previous string) (StoredToken, *tokenFailure) {
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
	out := attempt(ctx, c.http, attemptRequest{url: url, method: "POST", headers: headers, body: &body, timeout: c.tokenAttemptTimeout()})
	if out.kind != outcomeResponse || !out.bodyOK {
		return StoredToken{}, &tokenFailure{message: fmt.Sprintf("The OAuth2 token endpoint for %s could not be reached; check network access and the token URL.", scheme)}
	}
	decoded := decodeBody(out.body, out.headers, "application/json")
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
	return StoredToken{AccessToken: access, RefreshToken: refresh, ExpiresAt: expires, TokenType: stringAt("token_type"), Scope: stringAt("scope")}, nil
}

// clientCredentialsToken fetches (and caches) an OAuth2 client-credentials
// token.
func (c *ClientCore) clientCredentialsToken(ctx context.Context, s *satisfier) (string, string) {
	c.ccMu.Lock()
	defer c.ccMu.Unlock()
	realNow := time.Now().UnixMilli()
	if cached, ok := c.ccTokens[s.name]; ok && cached.expiresAt > realNow+30000 {
		return cached.token, ""
	}
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
	c.ccTokens[s.name] = cachedToken{token: token, expiresAt: realNow + lifetime}
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

// PKCE returns a fresh pair: pass Challenge to AuthorizationURL and keep
// Verifier for ExchangeCode.
func (f *OAuthFlow) PKCE() (PKCE, error) {
	p, ok := GeneratePKCE()
	if !ok {
		return PKCE{}, oauthError(f.operation("pkce"), "The system has no source of randomness for a PKCE verifier.")
	}
	return p, nil
}

// AuthorizationURL is the URL to send the user to.
func (f *OAuthFlow) AuthorizationURL(p AuthorizationURLParams) (string, error) {
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

// ExchangeCode trades the code from the redirect for tokens, stored for
// later calls.
func (f *OAuthFlow) ExchangeCode(ctx context.Context, p ExchangeCodeParams) (TokenInfo, error) {
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

// Refresh refreshes the stored token now. Calls refresh on their own when
// it expires.
func (f *OAuthFlow) Refresh(ctx context.Context) (TokenInfo, error) {
	operation := f.operation("refresh")
	if _, failure := f.core.storedTokenInner(ctx, f.scheme, true); failure != nil {
		return TokenInfo{}, oauthError(operation, failure.message)
	}
	stored, ok := f.core.readToken(f.scheme)
	if !ok {
		return TokenInfo{}, oauthError(operation, "No token is stored for "+f.scheme+".")
	}
	return tokenInfo(f.scheme, stored), nil
}

func tokenInfo(scheme string, t StoredToken) TokenInfo {
	return TokenInfo{Scheme: scheme, ExpiresAt: t.ExpiresAt, Scope: t.Scope, Refreshable: t.RefreshToken != ""}
}
