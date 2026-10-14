// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.Map;

/**
 * The HTTP answer of a call.
 *
 * @param status the status (0 for a local result)
 * @param headers the headers with lower-cased names; repeated values joined with ", "
 *     (set-cookie keeps its last value)
 * @param requestId the server's request id, or null
 * @param attempts the attempts made
 */
public record ResponseMeta(int status, Map<String, String> headers, String requestId, int attempts) {
    /** The answer of a call that sent nothing. */
    static ResponseMeta local() {
        return new ResponseMeta(0, Map.of(), null, 0);
    }
}
