// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"context"
	"fmt"
	"strings"
)

// Auth resolution: an operation's security is an OR of AND-sets of scheme
// names; the first alternative the configured credentials fully satisfy is
// applied. A composite profile satisfies the names in its Satisfies list
// when every part it needs for this request is configured.

type planHeader struct {
	name   string
	value  string
	secret bool
}

type authPlan struct {
	headers []planHeader
	cookies []pair
	query   []pair
}

type satisfierKind int

const (
	satComposite satisfierKind = iota
	satSecret
	satOAuthCode
	satOAuth
)

type satisfier struct {
	kind     satisfierKind
	name     string
	parts    []CompositePart
	values   map[string]string
	mutation bool
	scheme   *AuthScheme
	secret   string
	// clientSecret of an authorization-code or client-credentials client.
	clientSecret string
	clientID     string
	tokenURL     string
	scopes       []string
}

func isSafeMethod(method string) bool {
	switch method {
	case "GET", "HEAD", "OPTIONS", "TRACE":
		return true
	}
	return false
}

func compositeConfigKey(p CompositePart) string {
	switch p.Kind {
	case "cookie":
		return p.Name
	case "bearer":
		return "bearer"
	}
	if p.EqualsCookie != "" {
		return p.EqualsCookie
	}
	if p.FromConfig != "" {
		return p.FromConfig
	}
	return p.Name
}

func checkPrefix(token, prefix, what string) string {
	if prefix != "" && !strings.HasPrefix(token, prefix) {
		return fmt.Sprintf("The %s credential must start with \"%s\"; check that the right kind of token is configured.", what, prefix)
	}
	return ""
}

func skips(p CompositePart, mutation bool) bool {
	return p.Kind == "header" && p.MutationOnly && !mutation
}

func tryComposite(name string, parts []CompositePart, cred Credential, method string) (string, *satisfier, []string) {
	if !cred.isParts {
		return "", nil, []string{fmt.Sprintf("auth.%s (an object for the composite profile %s)", name, name)}
	}
	mutation := !isSafeMethod(method)
	var missing []string
	for _, p := range parts {
		if skips(p, mutation) {
			continue
		}
		key := compositeConfigKey(p)
		if cred.parts[key] == "" {
			var role string
			switch p.Kind {
			case "cookie":
				role = "cookie " + p.Name
			case "bearer":
				role = "bearer token"
			default:
				role = "header " + p.Name
				if p.EqualsCookie != "" {
					role += " (equal to cookie " + p.EqualsCookie + ")"
				}
			}
			missing = append(missing, fmt.Sprintf("auth.%s[%s] (%s of the composite profile %s)", name, jsString(key), role, name))
		}
	}
	if len(missing) > 0 {
		return "", nil, missing
	}
	return "composite:" + name, &satisfier{kind: satComposite, name: name, parts: parts, values: cred.parts, mutation: mutation}, nil
}

func credPart(parts map[string]string, snake, camel string) (string, bool) {
	if v, ok := parts[snake]; ok {
		return v, true
	}
	v, ok := parts[camel]
	return v, ok
}

func tryDirect(scheme *AuthScheme, cred Credential, configured bool) (string, *satisfier, []string) {
	name := scheme.Name
	if scheme.Kind == "oauth2" && configured && cred.isParts {
		parts := cred.parts
		if flow, _ := credPart(parts, "flow", "flow"); flow == "authorizationCode" {
			if scheme.AuthorizationCode == nil {
				return "", nil, []string{fmt.Sprintf("auth.%s (sets flow \"authorizationCode\", but the API descriptor has no authorizationCode flow for %s)", name, name)}
			}
			if id, _ := credPart(parts, "client_id", "clientId"); id == "" {
				return "", nil, []string{fmt.Sprintf("auth.%s (a client_id for the authorizationCode flow)", name)}
			}
			secret, _ := credPart(parts, "client_secret", "clientSecret")
			return "scheme:" + name, &satisfier{kind: satOAuthCode, name: name, clientSecret: secret}, nil
		}
		id, hasID := credPart(parts, "client_id", "clientId")
		secret, hasSecret := credPart(parts, "client_secret", "clientSecret")
		if hasID && hasSecret && scheme.TokenURL != "" {
			return "scheme:" + name, &satisfier{kind: satOAuth, name: name, tokenURL: scheme.TokenURL, scopes: scheme.Scopes, clientID: id, clientSecret: secret}, nil
		}
		code := ""
		if scheme.AuthorizationCode != nil {
			code = ", or {flow: \"authorizationCode\", client_id, client_secret?, redirect_uri?}"
		}
		return "", nil, []string{fmt.Sprintf("auth.%s (an access token, or {client_id, client_secret} for the token URL%s)", name, code)}
	}
	if configured && !cred.isParts && cred.secret != "" {
		return "scheme:" + name, &satisfier{kind: satSecret, scheme: scheme, secret: cred.secret}, nil
	}
	var what string
	switch scheme.Kind {
	case "http_basic":
		what = fmt.Sprintf("\"user:password\" for HTTP Basic scheme %s", name)
	case "api_key":
		place := string(scheme.In)
		what = fmt.Sprintf("API key for scheme %s (%s %s)", name, place, scheme.Wire)
	default:
		what = "bearer token for scheme " + name
	}
	return "", nil, []string{fmt.Sprintf("auth.%s (%s)", name, what)}
}

func tryScheme(api *APIDescriptor, auth AuthConfig, name, method string) (string, *satisfier, []string) {
	var missing []string
	for i := range api.Auth {
		s := &api.Auth[i]
		if s.Kind != "composite" || !containsString(s.Satisfies, name) {
			continue
		}
		cred, ok := auth[s.Name]
		if !ok {
			missing = append(missing, fmt.Sprintf("auth.%s (composite profile %s)", s.Name, s.Name))
			continue
		}
		key, sat, more := tryComposite(s.Name, s.Parts, cred, method)
		if sat != nil {
			return key, sat, nil
		}
		missing = append(missing, more...)
	}
	var direct *AuthScheme
	for i := range api.Auth {
		if api.Auth[i].Name == name && api.Auth[i].Kind != "composite" {
			direct = &api.Auth[i]
			break
		}
	}
	if direct != nil {
		cred, ok := auth[name]
		key, sat, more := tryDirect(direct, cred, ok)
		if sat != nil {
			return key, sat, nil
		}
		// A composite profile covering the name explains the gap better.
		if len(missing) == 0 {
			missing = append(missing, more...)
		}
	} else if len(missing) == 0 {
		missing = append(missing, fmt.Sprintf("a scheme named %s (the API descriptor does not define it)", name))
	}
	return "", nil, missing
}

func containsString(list []string, s string) bool {
	for _, x := range list {
		if x == s {
			return true
		}
	}
	return false
}

func configured(api *APIDescriptor, auth AuthConfig, name string) bool {
	if _, ok := auth[name]; ok {
		return true
	}
	for _, s := range api.Auth {
		if s.Kind == "composite" && containsString(s.Satisfies, name) {
			if _, ok := auth[s.Name]; ok {
				return true
			}
		}
	}
	return false
}

func bearer(plan *authPlan, token string) {
	plan.headers = append(plan.headers, planHeader{"Authorization", "Bearer " + token, true})
}

type authResult struct {
	plan    authPlan
	secrets []string
	// oauth is the authorization-code scheme whose stored token the plan
	// carries.
	oauth string
	// failed is the remediation of an AUTH_FAILED envelope.
	failed string
}

func (c *ClientCore) apply(ctx context.Context, s *satisfier, plan *authPlan, secrets *[]string, oauth *string) string {
	switch s.kind {
	case satComposite:
		for _, p := range s.parts {
			if skips(p, s.mutation) {
				continue
			}
			value := s.values[compositeConfigKey(p)]
			switch p.Kind {
			case "cookie":
				plan.cookies = append(plan.cookies, pair{p.Name, value})
				*secrets = append(*secrets, value)
			case "bearer":
				if problem := checkPrefix(value, p.Prefix, s.name+" bearer"); problem != "" {
					return problem
				}
				bearer(plan, value)
				*secrets = append(*secrets, value)
			default:
				secret := p.FromConfig == ""
				plan.headers = append(plan.headers, planHeader{p.Name, value, secret})
				if secret {
					*secrets = append(*secrets, value)
				}
			}
		}
	case satSecret:
		*secrets = append(*secrets, s.secret)
		switch s.scheme.Kind {
		case "api_key":
			switch s.scheme.In {
			case InQuery:
				plan.query = append(plan.query, pair{s.scheme.Wire, s.secret})
			case InCookie:
				plan.cookies = append(plan.cookies, pair{s.scheme.Wire, s.secret})
			default:
				plan.headers = append(plan.headers, planHeader{s.scheme.Wire, s.secret, true})
			}
		case "http_bearer":
			if problem := checkPrefix(s.secret, s.scheme.Prefix, s.scheme.Name); problem != "" {
				return problem
			}
			bearer(plan, s.secret)
		case "http_basic":
			encoded := base64Standard([]byte(s.secret))
			*secrets = append(*secrets, encoded)
			plan.headers = append(plan.headers, planHeader{"Authorization", "Basic " + encoded, true})
		default:
			bearer(plan, s.secret)
		}
	case satOAuthCode:
		if s.clientSecret != "" {
			*secrets = append(*secrets, s.clientSecret)
		}
		token, problem := c.storedToken(ctx, s.name, false)
		if problem != "" {
			return problem
		}
		*secrets = append(*secrets, token)
		bearer(plan, token)
		if *oauth == "" {
			*oauth = s.name
		}
	case satOAuth:
		*secrets = append(*secrets, s.clientSecret)
		token, problem := c.clientCredentialsToken(ctx, s)
		if problem != "" {
			return problem
		}
		*secrets = append(*secrets, token)
		bearer(plan, token)
	}
	return ""
}

// resolveAuth resolves the credentials for an operation. A problem is the
// result's failed remediation.
func (c *ClientCore) resolveAuth(ctx context.Context, op *OperationDescriptor, method string) authResult {
	api, auth := c.api, c.auth
	if len(op.Security) == 0 {
		return authResult{}
	}
	var best []string
	bestTouched, haveBest := false, false
	// An empty alternative (anonymous access) is the fallback, never
	// preferred over configured credentials.
	var ordered [][]string
	for _, alt := range op.Security {
		if len(alt) > 0 {
			ordered = append(ordered, alt)
		}
	}
	for _, alt := range op.Security {
		if len(alt) == 0 {
			ordered = append(ordered, alt)
		}
	}
	for _, alternative := range ordered {
		type found struct {
			key string
			sat *satisfier
		}
		var sats []found
		var missing []string
		for _, name := range alternative {
			key, sat, more := tryScheme(api, auth, name, method)
			if sat == nil {
				missing = append(missing, more...)
				continue
			}
			replaced := false
			for i := range sats {
				if sats[i].key == key {
					sats[i].sat = sat
					replaced = true
				}
			}
			if !replaced {
				sats = append(sats, found{key, sat})
			}
		}
		if len(missing) == 0 {
			var out authResult
			for _, f := range sats {
				if problem := c.apply(ctx, f.sat, &out.plan, &out.secrets, &out.oauth); problem != "" {
					return authResult{failed: fmt.Sprintf("%s Operation %s was not sent.", problem, op.ID)}
				}
			}
			return out
		}
		var unique []string
		for _, m := range missing {
			if !containsString(unique, m) {
				unique = append(unique, m)
			}
		}
		touched := false
		for _, name := range alternative {
			if configured(api, auth, name) {
				touched = true
			}
		}
		better := !haveBest || (touched && !bestTouched) || (touched == bestTouched && len(unique) < len(best))
		if better {
			best, bestTouched, haveBest = unique, touched, true
		}
	}
	alternatives := make([]string, len(op.Security))
	for i, alt := range op.Security {
		if len(alt) == 0 {
			alternatives[i] = "no credentials"
		} else {
			alternatives[i] = strings.Join(alt, " AND ")
		}
	}
	missingText := "credentials"
	if haveBest {
		missingText = strings.Join(best, "; ")
	}
	return authResult{failed: fmt.Sprintf("Operation %s needs %s. Missing: %s. Configure it in ClientOptions.auth and call again; the request was not sent.",
		op.ID, strings.Join(alternatives, " OR "), missingText)}
}
