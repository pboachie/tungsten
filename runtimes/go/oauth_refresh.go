// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"time"
)

type oauthRefreshKey struct {
	scheme  string
	version oauthVersion
}
type oauthRefreshResult struct {
	stored    StoredToken
	version   oauthVersion
	refreshed bool
	failure   *tokenFailure
}
type oauthRefresh struct {
	done     chan struct{}
	ctx      context.Context
	cancel   context.CancelFunc
	force    bool
	rejected string
	result   oauthRefreshResult
	deadline time.Time
}

func oauthChanged() *tokenFailure {
	return &tokenFailure{message: "OAuth session changed; authorize again."}
}
func oauthCanceled() *tokenFailure {
	return &tokenFailure{message: "OAuth token acquisition was canceled or exceeded its deadline."}
}

// Every managed store read is inside a bounded owner too: a synchronous custom
// Get may ignore cancellation just as a custom transport or final mutation may.
func (c *ClientCore) managedStoredToken(ctx context.Context, scheme string, force bool, rejected string) (StoredToken, *tokenFailure) {
	if !oauthText(scheme, maxOAuthCaptureBytes) || ctx.Err() != nil {
		return StoredToken{}, oauthCanceled()
	}
	if flow, _ := c.codeFlow(scheme); flow == nil {
		return StoredToken{}, oauthChanged()
	}
	for {
		c.oauth.mu.Lock()
		s := c.oauthSessionLocked(scheme)
		version := oauthVersion{s.generation, s.epoch}
		if !c.oauthCurrentLocked(scheme, version) {
			c.oauth.mu.Unlock()
			return StoredToken{}, oauthChanged()
		}
		key := oauthRefreshKey{scheme, version}
		flight := c.oauth.refreshes[key]
		owner := flight == nil
		if owner {
			if len(c.oauth.refreshes) >= maxOAuthAttempts {
				c.oauth.mu.Unlock()
				return StoredToken{}, &tokenFailure{message: "OAuth refresh capacity is exhausted; wait for owned token work to settle."}
			}
			// Background ownership prevents one waiter's cancellation from aborting
			// another. Logout explicitly cancels the owner; its slot stays charged.
			workCtx, cancel := context.WithTimeout(context.Background(), c.tokenAttemptTimeout())
			deadline, _ := workCtx.Deadline()
			flight = &oauthRefresh{done: make(chan struct{}), ctx: workCtx, cancel: cancel, force: force, rejected: rejected, deadline: deadline}
			if c.oauth.refreshes == nil {
				c.oauth.refreshes = map[oauthRefreshKey]*oauthRefresh{}
			}
			c.oauth.refreshes[key] = flight
		} else if force {
			flight.force = true
		}
		c.oauth.mu.Unlock()
		if owner {
			go c.runOAuthRefresh(key, flight)
		}
		select {
		case <-ctx.Done():
			return StoredToken{}, oauthCanceled()
		case <-flight.ctx.Done():
			// Normal retirement cancels only after publishing its result.
			select {
			case <-flight.done:
			default:
				return StoredToken{}, oauthCanceled()
			}
		case <-flight.done:
		}
		if ctx.Err() != nil {
			return StoredToken{}, oauthCanceled()
		}
		result := flight.result
		if result.failure != nil {
			return StoredToken{}, result.failure
		}
		if !time.Now().Before(flight.deadline) {
			return StoredToken{}, oauthCanceled()
		}
		if !c.oauthCurrent(scheme, result.version) {
			return StoredToken{}, oauthChanged()
		}
		// A forced caller can arrive after a fresh-token owner already decided
		// to return. It retries admission rather than treating that read as refresh.
		if force && !result.refreshed && (rejected == "" || result.stored.AccessToken == rejected) {
			continue
		}
		return cloneStoredToken(result.stored), nil
	}
}

func (c *ClientCore) runOAuthRefresh(key oauthRefreshKey, flight *oauthRefresh) {
	result := oauthRefreshResult{failure: &tokenFailure{message: "OAuth token storage or refresh did not complete; authorize again."}}
	defer func() {
		if recover() != nil {
			result = oauthRefreshResult{failure: &tokenFailure{message: "OAuth token storage or refresh did not complete; authorize again."}}
		}
		c.oauth.mu.Lock()
		flight.result = result
		if c.oauth.refreshes[key] == flight {
			delete(c.oauth.refreshes, key)
		}
		close(flight.done)
		c.oauth.mu.Unlock()
		flight.cancel()
	}()
	if !oauthBeforeDeadline(flight.ctx) || !c.oauthCurrent(key.scheme, key.version) {
		result.failure = oauthChanged()
		return
	}
	previous, ok := c.tokenStore.Get(key.scheme)
	previous = cloneStoredToken(previous)
	if !oauthBeforeDeadline(flight.ctx) {
		result.failure = oauthCanceled()
		return
	}
	if !c.oauthCurrent(key.scheme, key.version) {
		result.failure = oauthChanged()
		return
	}
	if !ok || previous.AccessToken == "" {
		result.failure = &tokenFailure{message: "No OAuth2 token is stored; begin a managed AuthorizationURL attempt and complete its bound full callback."}
		return
	}
	// The epoch clock is an application callback, so invoke it without oauth.mu.
	now := c.now()
	if !oauthBeforeDeadline(flight.ctx) {
		result.failure = oauthCanceled()
		return
	}
	c.oauth.mu.Lock()
	force := flight.force
	rejected := flight.rejected
	c.oauth.mu.Unlock()
	fresh := previous.ExpiresAt == nil || *previous.ExpiresAt > now+ExpirySkewMs
	if (rejected != "" && previous.AccessToken != rejected) || (fresh && !force && rejected == "") {
		result = oauthRefreshResult{stored: previous, version: key.version}
		return
	}
	result = c.refreshManagedStored(key, flight, previous)
}

func sameOAuthToken(current, previous StoredToken, ok bool) bool {
	return ok && current.AccessToken == previous.AccessToken && current.RefreshToken == previous.RefreshToken
}

func (c *ClientCore) refreshManagedStored(key oauthRefreshKey, flight *oauthRefresh, previous StoredToken) oauthRefreshResult {
	failure := func(problem *tokenFailure) oauthRefreshResult { return oauthRefreshResult{failure: problem} }
	fenced, ok := c.tokenStore.(FencedTokenStore)
	if !ok {
		return failure(&tokenFailure{message: "OAuth refresh requires an atomic fenced token store."})
	}
	if previous.RefreshToken == "" {
		return failure(&tokenFailure{message: "The OAuth token has no refresh token; begin a new managed authorization attempt."})
	}
	flow, reason := c.codeFlow(key.scheme)
	client, why := c.codeClient(key.scheme)
	if flow == nil || reason != "" || why != "" {
		return failure(oauthChanged())
	}
	endpoint := flow.RefreshURL
	if endpoint == "" {
		endpoint = flow.TokenURL
	}
	if !qualifiedOAuthEndpoint(endpoint, false) || insecureHTTP(endpoint, c.allowInsecureHTTP) || !validOAuthTokenLimit(c.maxOAuthTokenResponseBytes) {
		return failure(&tokenFailure{message: "OAuth refresh requires a bounded valid endpoint without protected query fields and a finite token response limit."})
	}
	// Capture the configuration used for this dispatch. Store callbacks never
	// select endpoints or consult mutable caller collections after admission.
	b := oauthBudget{}
	for _, value := range []string{key.scheme, flow.AuthorizationURL, flow.TokenURL, flow.RefreshURL, client.clientID, client.clientSecret, client.redirectURI} {
		if !b.charge(value) {
			return failure(oauthChanged())
		}
	}
	scopes, ok := b.scopes(flow.Scopes)
	if !ok {
		return failure(oauthChanged())
	}
	pinned := &pendingAuthorization{scheme: key.scheme, client: client, flow: AuthorizationCodeFlow{AuthorizationURL: flow.AuthorizationURL, TokenURL: flow.TokenURL, RefreshURL: flow.RefreshURL, Scopes: scopes}}
	if !oauthBeforeDeadline(flight.ctx) || !c.oauthCurrent(key.scheme, key.version) || !c.oauthConfigurationMatches(pinned) {
		return failure(oauthChanged())
	}
	fields := []pair{{"grant_type", "refresh_token"}, {"refresh_token", previous.RefreshToken}}
	renewed, problem := c.tokenRequest(flight.ctx, key.scheme, endpoint, client, fields, previous.RefreshToken)
	if !oauthBeforeDeadline(flight.ctx) {
		return failure(oauthCanceled())
	}
	current := func(stored StoredToken, exists bool) bool {
		return oauthBeforeDeadline(flight.ctx) && c.oauthCurrentLocked(key.scheme, key.version) && sameOAuthToken(stored, previous, exists) && c.oauthConfigurationMatches(pinned)
	}
	if problem != nil {
		if problem.rejected {
			fenced.DeleteIfCurrent(key.scheme, func(stored StoredToken, exists bool) bool {
				c.oauth.mu.Lock()
				defer c.oauth.mu.Unlock()
				if !current(stored, exists) {
					return false
				}
				_, ok := c.oauthMutateLocked(key.scheme)
				return ok
			})
		}
		return failure(problem)
	}
	var committed oauthVersion
	changed := fenced.SetIfCurrent(key.scheme, renewed, func(stored StoredToken, exists bool) bool {
		c.oauth.mu.Lock()
		defer c.oauth.mu.Unlock()
		if !current(stored, exists) {
			return false
		}
		epoch, ok := c.oauthMutateLocked(key.scheme)
		committed = oauthVersion{key.version.generation, epoch}
		return ok
	})
	if !changed || !oauthBeforeDeadline(flight.ctx) || !c.oauthCurrent(key.scheme, committed) {
		return failure(oauthChanged())
	}
	return oauthRefreshResult{stored: renewed, version: committed, refreshed: true}
}

func (c *ClientCore) storedToken(ctx context.Context, scheme string, force bool) (string, string) {
	stored, failure := c.managedStoredToken(ctx, scheme, force, "")
	if failure != nil {
		return "", failure.message
	}
	return stored.AccessToken, ""
}

func (c *ClientCore) refreshAfterRejection(ctx context.Context, scheme, rejected string) (string, bool) {
	stored, failure := c.managedStoredToken(ctx, scheme, true, rejected)
	return stored.AccessToken, failure == nil
}

// Refresh refreshes the stored token now. Ordinary calls refresh expired tokens
// automatically; waiters may cancel independently of their shared refresh owner.
func (f *OAuthFlow) Refresh(ctx context.Context) (TokenInfo, error) {
	stored, failure := f.core.managedStoredToken(ctx, f.scheme, true, "")
	if failure != nil {
		return TokenInfo{}, oauthError(f.operation("refresh"), failure.message)
	}
	return tokenInfo(f.scheme, stored), nil
}

// Wall-clock ClientOptions.Now controls token expiry only.
func oauthBeforeDeadline(ctx context.Context) bool {
	deadline, ok := ctx.Deadline()
	return ctx.Err() == nil && (!ok || time.Now().Before(deadline))
}
