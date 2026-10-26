// SPDX-License-Identifier: AGPL-3.0-only
package example.petstore;

/** A failed call: the message is the runtime's diagnostic envelope as JSON. */
public final class PetstoreException extends RuntimeException {
    private static final long serialVersionUID = 1L;

    public PetstoreException(String envelope) {
        super(envelope);
    }

    /** The diagnostic envelope as JSON (category, code, remediation, ...). */
    public String envelope() {
        return getMessage();
    }
}
