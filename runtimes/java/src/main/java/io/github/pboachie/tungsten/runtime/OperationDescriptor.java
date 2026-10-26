// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.List;
import java.util.Map;

/**
 * Everything the runtime needs to call one operation, read from the SDK descriptor document
 * ({@link Descriptors}). Arguments objects are keyed by the arguments layout keys ({@link
 * Param#name}, {@link MergedField#arg}).
 *
 * @param id the operation id
 * @param method the HTTP method
 * @param path the path template; placeholders are wire names
 * @param params the parameters: arguments first, then the supplied ones
 * @param body the request body, or null
 * @param responses the declared responses
 * @param security an OR of AND-sets of auth scheme names; empty means no auth
 * @param pagination how the operation pages, or null
 * @param rpc the rpc envelope, or null
 * @param errorCodeField the path of the API error code in error bodies, or null
 * @param gate the runtime gate, or null
 * @param agent the agent metadata
 * @param request validates the arguments object before any network call, or null
 * @param response validates a success body, or null
 * @param pageItem validates each item of a page, or null
 * @param stream the server-sent events, or null
 * @param summary the one-line summary, or null
 */
public record OperationDescriptor(
        String id,
        String method,
        String path,
        List<Param> params,
        Body body,
        List<Response> responses,
        List<List<String>> security,
        Pagination pagination,
        Rpc rpc,
        String errorCodeField,
        Gate gate,
        Agent agent,
        Schema request,
        Schema response,
        Schema pageItem,
        Stream stream,
        String summary) {

    /**
     * One parameter.
     *
     * @param name the key in the arguments object; informational for supplied parameters
     * @param wire the name on the wire
     * @param in path, query, header or cookie
     * @param required whether it must be given
     * @param style the OpenAPI style (simple, form, label, matrix, space_delimited,
     *     pipe_delimited, deep_object, cookie)
     * @param explode the OpenAPI explode flag
     * @param role plain, dry_run, idempotency_key, origin, auth or constant
     * @param constant the header value of a constant parameter, or null
     * @param sensitive whether the value is a secret
     */
    public record Param(
            String name,
            String wire,
            String in,
            boolean required,
            String style,
            boolean explode,
            String role,
            String constant,
            boolean sensitive) {
        /** Whether the caller supplies it as an argument (plain or dry_run). */
        public boolean isArgument() {
            return "plain".equals(role) || "dry_run".equals(role);
        }
    }

    /**
     * One field of a merged body.
     *
     * @param arg the key in the arguments object
     * @param wire the name on the wire
     */
    public record MergedField(String arg, String wire) {}

    /**
     * The request body: with {@code arg} set the whole body is that argument, otherwise a JSON
     * object of the merged fields.
     *
     * @param mediaType the media type
     * @param encoding json, form, multipart, bytes or text
     * @param required whether a body must be sent
     * @param fields the merged fields
     * @param arg the argument holding the whole body, or null
     * @param partContentTypes the media type each multipart field is sent with (OpenAPI
     *     {@code encoding}), by wire name; a file part's own media type wins
     */
    public record Body(
            String mediaType,
            String encoding,
            boolean required,
            List<MergedField> fields,
            String arg,
            Map<String, String> partContentTypes) {
        /**
         * A body without declared part encodings.
         *
         * @param mediaType the media type
         * @param encoding json, form, multipart, bytes or text
         * @param required whether a body must be sent
         * @param fields the merged fields
         * @param arg the argument holding the whole body, or null
         */
        public Body(String mediaType, String encoding, boolean required, List<MergedField> fields, String arg) {
            this(mediaType, encoding, required, fields, arg, Map.of());
        }
    }

    /**
     * One declared response.
     *
     * @param statusKind exact, class or default
     * @param statusCode the status (exact) or the hundreds digit (class)
     * @param kind success, error or ambiguous
     * @param mediaType the first media type, or null for a bare status
     */
    public record Response(String statusKind, int statusCode, String kind, String mediaType) {}

    /**
     * How an operation pages; parameter names are arguments keys.
     *
     * @param style cursor, offset, page or link_header
     * @param requestParam the cursor argument
     * @param responseField the response field with the next cursor
     * @param itemsField the response field with the items ("" for the body)
     * @param pageSizeParam the page size argument
     * @param hasMoreField iteration stops when this response boolean is false
     * @param cursorItemField without a cursor in the response, this field of the last item
     * @param offsetParam the offset argument
     * @param limitParam the limit argument
     * @param pageParam the page number argument
     * @param sizeParam the page size argument of page pagination
     */
    public record Pagination(
            String style,
            String requestParam,
            String responseField,
            String itemsField,
            String pageSizeParam,
            String hasMoreField,
            String cursorItemField,
            String offsetParam,
            String limitParam,
            String pageParam,
            String sizeParam) {}

    /**
     * The rpc envelope of an unflattened rpc operation.
     *
     * @param field the envelope member set to value
     * @param value the method name
     * @param paramsField the member holding the arguments
     * @param constants members sent verbatim, or null
     */
    public record Rpc(String field, String value, String paramsField, JsonObject constants) {}

    /**
     * A runtime gate: the operation is mounted only when the variable is on.
     *
     * @param envVar the deployment variable
     * @param disabledStatus the status the server answers when it is off
     */
    public record Gate(String envVar, int disabledStatus) {}

    /**
     * A remediation entry; null members are unset.
     *
     * @param category the category, or null
     * @param text the remediation, or null
     * @param retryable the retryable, or null
     * @param nextAction the next action, or null
     */
    public record Remediation(Category category, String text, Retryable retryable, String nextAction) {}

    /**
     * An idempotency policy.
     *
     * @param policy none, auto, caller_owned, content_hash or content_identity
     * @param header the key header, or null
     * @param format the key format, or null
     * @param persistRequired whether the caller must persist a key
     * @param note a note for remediation texts, or null
     */
    public record Idempotency(
            String policy, String header, String format, boolean persistRequired, String note) {}

    /**
     * How preview works.
     *
     * @param mode local, header, endpoint or none
     * @param header the dry-run header (header mode), or null
     * @param value the dry-run header value, or null
     * @param operation the preview operation (endpoint mode), or null
     */
    public record Preview(String mode, String header, String value, String operation) {}

    /**
     * The confirmation text.
     *
     * @param summaryFields the arguments the preview shows
     * @param message the message with {field} references, or null
     */
    public record Confirmation(List<String> summaryFields, String message) {}

    /**
     * A verification hook: after success, call the operation with args and compare with expect;
     * poll until terminal holds.
     *
     * @param operation the read to call
     * @param args its arguments (wire names; $response. and $args. references)
     * @param expect the predicate that must hold
     * @param terminal the predicate polling waits for
     * @param pollIntervalMs the poll interval, or null
     * @param pollBudgetMs the poll budget, or null
     */
    public record Verify(
            String operation,
            Object args,
            Object expect,
            Object terminal,
            Long pollIntervalMs,
            Long pollBudgetMs) {}

    /**
     * The agent metadata of an operation.
     *
     * @param safety the tier
     * @param idempotency the idempotency policy
     * @param preview how preview works
     * @param confirmation the confirmation text, or null
     * @param verify the verification hook, or null
     * @param remediation operation-specific remediation by API error code
     * @param remediationNote a note shown in previews, or null
     * @param sensitiveResponseFields response fields carrying secrets
     * @param sensitiveRequestFields dotted argument paths carrying secrets
     * @param shownOnce whether the response holds values the API shows only once
     * @param importPolicy the review policy of an operation imported from a
     *     collection or capture, or null for an ordinary operation
     */
    public record Agent(
            Safety safety,
            Idempotency idempotency,
            Preview preview,
            Confirmation confirmation,
            Verify verify,
            Map<String, Remediation> remediation,
            String remediationNote,
            List<String> sensitiveResponseFields,
            List<String> sensitiveRequestFields,
            boolean shownOnce,
            ImportPolicy importPolicy) {

        /** The agent metadata of an operation without an import policy. */
        public Agent(
                Safety safety,
                Idempotency idempotency,
                Preview preview,
                Confirmation confirmation,
                Verify verify,
                Map<String, Remediation> remediation,
                String remediationNote,
                List<String> sensitiveResponseFields,
                List<String> sensitiveRequestFields,
                boolean shownOnce) {
            this(safety, idempotency, preview, confirmation, verify, remediation, remediationNote,
                    sensitiveResponseFields, sensitiveRequestFields, shownOnce, null);
        }
    }

    /**
     * The review policy of an operation imported from a Postman collection,
     * cURL command, HTTP request file or HAR capture. A reviewed operation may
     * be sent only to one of its allowed origins; an unreviewed one carries an
     * effective safety tier that requires preview and confirmation.
     *
     * @param allowedOrigins the origins (scheme, host, port) the review covers
     * @param inferredSafety the tier inferred from the HTTP method
     * @param reviewed whether a person reviewed the operation
     * @param source the kind of input the operation came from
     */
    public record ImportPolicy(
            List<String> allowedOrigins,
            Safety inferredSafety,
            boolean reviewed,
            String source) {}

    /**
     * The server-sent events of an operation.
     *
     * @param event validates each event's decoded data, or null
     * @param done a data value that ends the stream, or null
     * @param flag the wire name of the boolean body field that selects the stream, or null
     * @param text whether every event's data is text delivered as written, not JSON
     */
    public record Stream(Schema event, String done, String flag, boolean text) {
        /**
         * A stream whose event data is JSON.
         *
         * @param event validates each event's decoded data, or null
         * @param done a data value that ends the stream, or null
         * @param flag the wire name of the boolean body field that selects the stream, or null
         */
        public Stream(Schema event, String done, String flag) {
            this(event, done, flag, false);
        }
    }

    /** Whether the operation changes state (any tier but read_only). */
    public boolean isMutation() {
        return agent.safety() != Safety.READ_ONLY;
    }

    /** The parameters the caller supplies as arguments. */
    public List<Param> argumentParams() {
        return params.stream().filter(Param::isArgument).toList();
    }
}
