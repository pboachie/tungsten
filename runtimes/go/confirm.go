// SPDX-License-Identifier: Apache-2.0

package tungsten

import (
	"strconv"
	"strings"
)

// ConfirmationTTLMs is the lifetime of a confirmation token.
const ConfirmationTTLMs = 5 * 60 * 1000

const tokenPrefix = "tgc1"

// ArgsDigest is the SHA-256 of the canonical JSON of the arguments.
func ArgsDigest(args any) string {
	return sha256Hex([]byte(CanonicalJSON(args)))
}

func tokenPayload(operation, digest string, expiry int64) string {
	return operation + "\n" + digest + "\n" + strconv.FormatInt(expiry, 10)
}

func tokenSignature(key []byte, operation, digest string, expiry int64) string {
	return base64URL(hmacSHA256(key, []byte(tokenPayload(operation, digest, expiry))))
}

// IssueToken is tgc1.<expiry ms>.<base64url(HMAC-SHA-256(key, operation,
// args digest, expiry))>: valid only for the operation and the exact
// arguments, until its expiry.
func IssueToken(key []byte, operation string, args any, now int64) string {
	expiry := now + ConfirmationTTLMs
	return tokenPrefix + "." + strconv.FormatInt(expiry, 10) + "." + tokenSignature(key, operation, ArgsDigest(args), expiry)
}

type tokenCheck int

const (
	tokenValid tokenCheck = iota
	tokenExpired
	tokenMismatch
	tokenMalformed
)

// tokenExpiry is the expiry a token claims, when it has the token shape.
func tokenExpiry(token string) (int64, string, bool) {
	rest, ok := strings.CutPrefix(token, tokenPrefix+".")
	if !ok {
		return 0, "", false
	}
	expiry, signature, ok := strings.Cut(rest, ".")
	if !ok || expiry == "" || len(expiry) > 16 {
		return 0, "", false
	}
	for i := 0; i < len(expiry); i++ {
		if expiry[i] < '0' || expiry[i] > '9' {
			return 0, "", false
		}
	}
	if len(signature) != 43 {
		return 0, "", false
	}
	for i := 0; i < len(signature); i++ {
		c := signature[i]
		if !((c >= 'a' && c <= 'z') || (c >= 'A' && c <= 'Z') || (c >= '0' && c <= '9') || c == '_' || c == '-') {
			return 0, "", false
		}
	}
	n, err := strconv.ParseInt(expiry, 10, 64)
	if err != nil {
		return 0, "", false
	}
	return n, signature, true
}

func checkToken(key []byte, token, operation string, args any, now int64) tokenCheck {
	expiry, given, ok := tokenExpiry(token)
	if !ok {
		return tokenMalformed
	}
	if !timingSafeEqual(tokenSignature(key, operation, ArgsDigest(args), expiry), given) {
		return tokenMismatch
	}
	if expiry > now {
		return tokenValid
	}
	return tokenExpired
}
