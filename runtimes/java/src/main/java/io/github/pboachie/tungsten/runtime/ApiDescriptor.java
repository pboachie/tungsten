// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.List;
import java.util.Map;

/**
 * The API, read from the SDK descriptor document.
 *
 * @param name the machine name
 * @param version the SDK version
 * @param tungstenVersion the version of tungsten that generated the SDK
 * @param servers the server URLs
 * @param auth the auth schemes
 * @param errorCodes the global remediation by API error code
 * @param ambiguousStatuses statuses that make a mutation's outcome unknown
 * @param nonJson error answers without a JSON body
 * @param gates a gate's environment variable to its explanation
 * @param readOnlyRetries retry defaults of read-only operations
 * @param mutatingRetries retry defaults of the other tiers
 * @param redirects the maximum redirect authority
 */
public record ApiDescriptor(
        String name,
        String version,
        String tungstenVersion,
        List<String> servers,
        List<AuthScheme> auth,
        Map<String, OperationDescriptor.Remediation> errorCodes,
        List<Integer> ambiguousStatuses,
        List<NonJson> nonJson,
        Map<String, String> gates,
        RetryOptions readOnlyRetries,
        RetryOptions mutatingRetries,
        RedirectPolicy redirects) {

    /** Compatibility constructor: existing descriptors retain same-origin read redirects. */
    public ApiDescriptor(String name, String version, String tungstenVersion, List<String> servers,
            List<AuthScheme> auth, Map<String, OperationDescriptor.Remediation> errorCodes,
            List<Integer> ambiguousStatuses, List<NonJson> nonJson, Map<String, String> gates,
            RetryOptions readOnlyRetries, RetryOptions mutatingRetries) {
        this(name, version, tungstenVersion, servers, auth, errorCodes, ambiguousStatuses,
                nonJson, gates, readOnlyRetries, mutatingRetries, RedirectPolicy.SAME_ORIGIN_READ);
    }


    /**
     * One auth scheme. OpenID Connect schemes are {@code http_bearer}.
     *
     * @param kind api_key, http_bearer, http_basic, oauth2 or composite
     * @param name the scheme name
     * @param in where an API key travels (header, query, cookie), or null
     * @param wire the API key's wire name, or null
     * @param prefix the prefix a bearer token must start with, or null
     * @param tokenUrl the OAuth2 token URL, or null
     * @param scopes the OAuth2 scopes
     * @param authorizationCode the authorization-code flow, or null
     * @param satisfies the scheme names a composite profile satisfies
     * @param parts the parts of a composite profile
     */
    public record AuthScheme(
            String kind,
            String name,
            String in,
            String wire,
            String prefix,
            String tokenUrl,
            List<String> scopes,
            AuthorizationCode authorizationCode,
            List<String> satisfies,
            List<CompositePart> parts) {}

    /**
     * The authorization-code flow of an OAuth2 scheme.
     *
     * @param authorizationUrl where the user authorizes
     * @param tokenUrl where codes are exchanged
     * @param refreshUrl where tokens are refreshed (the token URL when null)
     * @param scopes the flow's scopes
     */
    public record AuthorizationCode(
            String authorizationUrl, String tokenUrl, String refreshUrl, List<String> scopes) {}

    /**
     * One part of a composite auth profile.
     *
     * @param kind cookie, header or bearer
     * @param name the cookie or header name
     * @param equalsCookie the cookie a header repeats, or null
     * @param fromConfig the configuration key of a header, or null
     * @param mutationOnly whether a header is sent on mutations only
     * @param prefix the prefix a bearer part must start with, or null
     */
    public record CompositePart(
            String kind,
            String name,
            String equalsCookie,
            String fromConfig,
            boolean mutationOnly,
            String prefix) {}

    /**
     * An error answer without a JSON body.
     *
     * @param status the status
     * @param media a media type, or "none" for a bare status
     * @param category the category
     * @param retryable the retryable
     * @param text the remediation, or null
     */
    public record NonJson(int status, String media, Category category, Retryable retryable, String text) {}
}
