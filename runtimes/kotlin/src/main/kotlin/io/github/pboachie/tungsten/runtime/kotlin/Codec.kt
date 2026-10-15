// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime.kotlin

import io.github.pboachie.tungsten.runtime.Binary
import io.github.pboachie.tungsten.runtime.Decode
import io.github.pboachie.tungsten.runtime.Json
import io.github.pboachie.tungsten.runtime.JsonObject
import io.github.pboachie.tungsten.runtime.Typed

/**
 * The JSON conversion helpers generated Kotlin data classes call: decoders `(Any?) -> T` over
 * the Java runtime's JSON values (JsonObject, List, String, Long, Double, Boolean, null) and
 * encoders `(T) -> Any?` back to them. Decoders throw the Java runtime's `Decode.Failure` with
 * the path of the member that did not decode; nothing here sends or validates a request.
 */
public object Codec {
    // ------------------------------------------------------------ scalars

    /** The value as it is (the encoder of scalars and of JSON values). */
    @JvmStatic
    public fun same(value: Any?): Any? = value

    /** Any JSON value (a copy). */
    @JvmStatic
    public fun json(value: Any?): Any? = Decode.any(value)

    /** A JSON object, as it is. */
    @JvmStatic
    public fun jsonObject(value: Any?): JsonObject = Decode.`object`(value)

    /** A JSON object, the receiver of member decoders. */
    @JvmStatic
    public fun obj(value: Any?): JsonObject = Decode.`object`(value)

    /** A string. */
    @JvmStatic
    public fun string(value: Any?): String = Decode.string(value)

    /** A boolean. */
    @JvmStatic
    public fun bool(value: Any?): Boolean = Decode.bool(value)

    /** A 64-bit integer. */
    @JvmStatic
    public fun long(value: Any?): Long = Decode.int64(value)

    /** A 32-bit integer. */
    @JvmStatic
    public fun int(value: Any?): Int = Decode.int32(value)

    /** A number. */
    @JvmStatic
    public fun double(value: Any?): Double = Decode.number(value)

    /** Base64 text as bytes. */
    @JvmStatic
    public fun bytes(value: Any?): ByteArray = Decode.bytes(value)

    /** Bytes as base64 text. */
    @JvmStatic
    public fun encodeBytes(value: ByteArray): Any? = Decode.encodeBytes(value)

    /** Binary data: the runtime's tagged binary object, or base64 text. */
    @JvmStatic
    public fun binary(value: Any?): Binary = Decode.binary(value)

    /** Binary data as the runtime's tagged binary object. */
    @JvmStatic
    public fun encodeBinary(value: Binary): Any? = value.toJson()

    // ------------------------------------------------------------ combinators

    /** null, or the value decoded by [decode]. */
    @JvmStatic
    public fun <T> nullable(value: Any?, decode: (Any?) -> T): T? = if (value == null) null else decode(value)

    /** An array of items decoded by [item]. */
    @JvmStatic
    public fun <T> list(value: Any?, item: (Any?) -> T): List<T> = Decode.list(value) { item(it) }

    /** A list as a JSON array; null items stay null. */
    @JvmStatic
    public fun <T : Any> encodeList(items: List<T?>, encode: (T) -> Any?): Any? = items.map { if (it == null) null else encode(it) }

    /** An object of member values decoded by [item], in order. */
    @JvmStatic
    public fun <T> map(value: Any?, item: (Any?) -> T): Map<String, T> = Decode.map(value) { item(it) }

    /** A map as a JSON object, in its order; null values stay null. */
    @JvmStatic
    public fun <T : Any> encodeMap(members: Map<String, T?>, encode: (T) -> Any?): Any? {
        val out = JsonObject()
        for ((k, v) in members) {
            out.set(k, if (v == null) null else encode(v))
        }
        return out
    }

    /** [decode] applied to [value]. */
    @JvmStatic
    public fun <T> apply(value: Any?, decode: (Any?) -> T): T = decode(value)

    /** [encode] applied to [value]; null stays null. */
    @JvmStatic
    public fun <T : Any> encode(value: T?, encode: (T) -> Any?): Any? = if (value == null) null else encode(value)

    /** A value that decoded, possibly null (the result of [attempt]). */
    public class Decoded<out T>(
        /** The decoded value. */
        public val value: T,
    )

    /** The value decoded by [decode], or null when it does not decode (a union variant tried in turn). */
    @JvmStatic
    public fun <T> attempt(value: Any?, decode: (Any?) -> T): Decoded<T>? =
        try {
            Decoded(decode(value))
        } catch (_: Decode.Failure) {
            null
        }

    // ------------------------------------------------------------ members

    /** A member: null when absent or null, else decoded (presence is the response schema's check). */
    @JvmStatic
    public fun <T> member(obj: JsonObject, name: String, decode: (Any?) -> T): T? = Decode.member(obj, name) { decode(it) }

    /** A member where null is a value: [Patch.Unset] when absent, [Patch.Null] for null, else the decoded value. */
    @JvmStatic
    public fun <T : Any> patch(obj: JsonObject, name: String, decode: (Any?) -> T): Patch<T> {
        if (!obj.has(name)) {
            return Patch.Unset
        }
        val raw = obj.get(name) ?: return Patch.Null
        return try {
            Patch.Value(decode(raw))
        } catch (f: Decode.Failure) {
            throw f.below(name)
        }
    }

    /** Refuses members outside [known] (a closed class). */
    @JvmStatic
    public fun closed(obj: JsonObject, known: Set<String>) {
        Decode.closed(obj, known)
    }

    /** The members outside [known], decoded by [decode], in order. */
    @JvmStatic
    public fun <T> extra(obj: JsonObject, known: Set<String>, decode: (Any?) -> T): Map<String, T> =
        Decode.extra(obj, known) { decode(it) }

    /** Sets a member when the value is not null. */
    @JvmStatic
    public fun <T : Any> put(obj: JsonObject, name: String, value: T?, encode: (T) -> Any?) {
        if (value != null) {
            obj.set(name, encode(value))
        }
    }

    /** Sets a member of a [Patch] when it is set: null for [Patch.Null], else the encoded value. */
    @JvmStatic
    public fun <T : Any> putPatch(obj: JsonObject, name: String, value: Patch<T>, encode: (T) -> Any?) {
        when (value) {
            is Patch.Value -> obj.set(name, encode(value.value))
            Patch.Null -> obj.set(name, null)
            Patch.Unset -> Unit
        }
    }

    /** Writes the additional members that are not already set, in order; null values stay null. */
    @JvmStatic
    public fun <T : Any> putExtra(obj: JsonObject, members: Map<String, T?>, encode: (T) -> Any?) {
        for ((k, v) in members) {
            if (!obj.has(k)) {
                obj.set(k, if (v == null) null else encode(v))
            }
        }
    }

    // ------------------------------------------------------------ documents

    /**
     * An arguments object decoded by [fromJson], or null when it does not decode (the caller then
     * passes the raw arguments to the runtime, so pre-flight validation reports them).
     */
    @JvmStatic
    public fun <T> decodeArgs(args: Any?, fromJson: (Any?) -> T): T? = Typed.decodeArgs(args) { fromJson(it) }

    /** The value as the runtime's JSON value (models, maps, lists and scalars). */
    @JvmStatic
    public fun toJsonValue(value: Any?): Any? = Json.normalize(value)

    /** The value as JSON text. */
    @JvmStatic
    public fun toJsonText(value: Any?): String = Json.write(Json.normalize(value))

    /** JSON text decoded by [fromJson] (a generated class's companion `fromJson`). */
    @JvmStatic
    public fun <T> fromJsonText(text: String, fromJson: (Any?) -> T): T = fromJson(Json.parse(text))
}

/** The model as JSON text. */
public fun Json.Encodable.toJsonText(): String = Codec.toJsonText(this)
