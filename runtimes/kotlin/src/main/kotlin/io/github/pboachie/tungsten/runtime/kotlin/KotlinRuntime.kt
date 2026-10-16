// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime.kotlin

import io.github.pboachie.tungsten.runtime.ClientOptions
import io.github.pboachie.tungsten.runtime.RuntimeIdentity

/**
 * The Kotlin layer's identity. A generated Kotlin client runs on the Java runtime and reports
 * `(kotlin)` in `User-Agent` and `tungsten-kt/<version>` in `X-Tungsten-Runtime`; the option
 * names in remediation prose stay the Java runtime's (`idempotencyKey`, `Confirm.YES`, ...),
 * which are the same names in Kotlin.
 */
public object KotlinRuntime {
    /** The version of the Kotlin layer. */
    public const val VERSION: String = "0.1.3"

    /** The User-Agent language token. */
    public const val LANGUAGE: String = "kotlin"

    /** The runtime id of X-Tungsten-Runtime. */
    public const val RUNTIME: String = "kt"

    /** The identity a generated Kotlin client reports. */
    @JvmField
    public val IDENTITY: RuntimeIdentity = RuntimeIdentity.builder(LANGUAGE, RUNTIME).version(VERSION).build()

    /**
     * The options a generated client builds its core with: the given options reporting
     * [IDENTITY], unless they were built with an identity other than the Java runtime's,
     * which is kept. Every other option, the confirmation key and the stores included, is
     * the given options'.
     */
    public fun options(options: ClientOptions): ClientOptions =
        if (options.identity() === RuntimeIdentity.JAVA) options.withIdentity(IDENTITY) else options

    /** Default client options reporting [IDENTITY]. */
    public fun options(): ClientOptions = ClientOptions.builder().identity(IDENTITY).build()

    /** Client options built by [configure] on a builder that reports [IDENTITY]. */
    public fun options(configure: ClientOptions.Builder.() -> Unit): ClientOptions =
        ClientOptions.builder().identity(IDENTITY).apply(configure).build()
}
