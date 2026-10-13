// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.List;

/**
 * A compiled macro, read from the SDK descriptor document. Arguments, budgets and the output
 * are expressions: strings starting with "$" reference $input or an earlier step's "as" name,
 * {"expr": "&lt;ref&gt; in [..]" | "&lt;ref&gt; == x" | "&lt;ref&gt; != x"} is a boolean,
 * anything else a literal.
 *
 * @param name the macro name
 * @param summary the summary
 * @param safety the declared tier (the run uses the highest tier of its steps)
 * @param steps the steps
 * @param output the output expression
 * @param input the input
 * @param sensitiveResponseFields output fields carrying secrets
 * @param shownOnce whether the output holds values the API returns only once
 * @param cluster the cluster, or null
 */
public record MacroDescriptor(
        String name,
        String summary,
        Safety safety,
        List<Step> steps,
        Object output,
        Input input,
        List<String> sensitiveResponseFields,
        boolean shownOnce,
        String cluster) {

    /**
     * One step.
     *
     * @param kind call, poll or paginate
     * @param operation the operation id
     * @param args the arguments expression
     * @param as the name the result is bound to, or null
     * @param until the predicate a poll waits for, or null
     * @param intervalMs the poll interval, or null
     * @param budgetMs the poll budget expression, or null
     * @param maxPages the page limit of a paginate step, or null
     */
    public record Step(
            String kind,
            String operation,
            Object args,
            String as,
            Object until,
            Long intervalMs,
            Object budgetMs,
            Long maxPages) {}

    /**
     * One input field.
     *
     * @param key the key in the input object
     * @param name the target's name
     * @param required whether it must be given
     */
    public record InputField(String key, String name, boolean required) {}

    /**
     * The macro's input.
     *
     * @param extendsOperation the operation whose arguments it takes, or null
     * @param add added fields as JSON Schemas (a default applies when the input omits one)
     * @param fields every input field
     */
    public record Input(String extendsOperation, JsonObject add, List<InputField> fields) {}
}
