// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.List;

/**
 * Implemented by every generated client: drivers that only know operation ids and JSON (the
 * contract suite, tool servers) run the same typed methods as application code. {@link #invoke}
 * decodes the arguments into the operation's request record, calls the typed method and returns
 * its result with the decoded wire body as the value. When the arguments do not decode, it
 * calls {@link ClientCore#call} with them, so the VALIDATION_FAILED envelope is the pre-flight
 * one. Arguments are keyed by the arguments layout keys.
 */
public interface Dispatch {
    /** The runtime client the SDK runs on. */
    ClientCore core();

    /** Every callable operation, in IR order. */
    List<OperationDescriptor> operations();

    /** Every macro, in IR order. */
    List<MacroDescriptor> macros();

    /** Runs an operation by id. */
    Result<Object> invoke(String operation, Object args, CallOptions opts);

    /** Previews an operation by id. */
    Result<PreviewResult> previewOperation(String operation, Object args, CallOptions opts);

    /**
     * Every page of a paginated operation, items as decoded JSON; a failure ends the list with
     * its error.
     */
    List<Result<Page<Object>>> paginate(String operation, Object args, CallOptions opts);

    /** The event stream of an operation; each event's raw value is the decoded JSON. */
    EventStream<?> streamEvents(String operation, Object args, CallOptions opts);

    /** Runs a macro by name. */
    Result<Object> runMacro(String name, Object input, CallOptions opts);

    /** Previews a macro by name without sending anything. */
    Result<PreviewResult> previewMacro(String name, Object input, CallOptions opts);

    /** VALIDATION_FAILED for an operation id the client does not have. */
    static <T> Result<T> unknownOperation(ClientCore core, String operation) {
        return core.diag(operation, Category.VALIDATION_FAILED).param("operation").expected("an operation id of this SDK")
                .remedy("This SDK has no operation " + operation + "; check the id (operations() lists them). Nothing was sent.")
                .fail().toResult();
    }

    /** VALIDATION_FAILED for a macro name the client does not have. */
    static <T> Result<T> unknownMacro(ClientCore core, String name) {
        return core.diag(name, Category.VALIDATION_FAILED).param("macro").expected("a macro name of this SDK")
                .remedy("This SDK has no macro " + name + "; check the name (macros() lists them). Nothing was sent.")
                .fail().toResult();
    }

    /** VALIDATION_FAILED for an operation without pagination. */
    static <T> Result<T> notPaginated(ClientCore core, String operation) {
        return core.diag(operation, Category.VALIDATION_FAILED).param("operation").expected("a paginated operation")
                .remedy(operation + " is not paginated; use invoke instead. Nothing was sent.").fail().toResult();
    }

    /** An event stream that fails at once: the operation has no event stream. */
    static EventStream<Object> noStream(ClientCore core, OperationDescriptor op, String operation) {
        TungstenException e = core.diag(operation, Category.VALIDATION_FAILED).param("operation")
                .expected("an operation with an event stream")
                .remedy(operation + " has no event stream (its success response is not text/event-stream); use invoke instead. Nothing was sent.")
                .fail();
        return new EventStream<>(core, op, null, CallOptions.defaults(), v -> v, e);
    }

    /** The operation with this id, or null. */
    static OperationDescriptor find(List<OperationDescriptor> operations, String id) {
        for (OperationDescriptor op : operations) {
            if (op.id().equals(id)) {
                return op;
            }
        }
        return null;
    }

    /** The macro with this name, or null. */
    static MacroDescriptor findMacro(List<MacroDescriptor> macros, String name) {
        for (MacroDescriptor m : macros) {
            if (m.name().equals(name)) {
                return m;
            }
        }
        return null;
    }
}
