// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.util.Base64;
import java.util.Objects;

/**
 * The value of a bytes or multipart body field: the bytes and, for a multipart part, its optional
 * file name and media type. Inside an arguments object (which is JSON) it travels as the tagged
 * object {@code {"$tungsten_binary": "<base64>", "filename": ..., "content_type": ...}}; only the
 * request encoder interprets it. Base64 text inside JSON models ({@code format: byte}) is a plain
 * {@code byte[]} field instead.
 */
public final class Binary implements Json.Encodable {
    /** The member that marks bytes inside an arguments object. */
    public static final String KEY = "$tungsten_binary";

    private final byte[] data;
    private final String filename;
    private final String contentType;

    private Binary(byte[] data, String filename, String contentType) {
        this.data = data.clone();
        this.filename = filename;
        this.contentType = contentType;
    }

    /** Wraps bytes. */
    public static Binary of(byte[] data) {
        return new Binary(Objects.requireNonNull(data, "data"), null, null);
    }

    /** The same bytes with a multipart file name. */
    public Binary withFilename(String name) {
        return new Binary(data, name, contentType);
    }

    /** The same bytes with the multipart part's media type. */
    public Binary withContentType(String media) {
        return new Binary(data, filename, media);
    }

    /** A copy of the bytes. */
    public byte[] data() {
        return data.clone();
    }

    /** The number of bytes. */
    public int length() {
        return data.length;
    }

    /** The multipart file name, or null. */
    public String filename() {
        return filename;
    }

    /** The multipart media type, or null. */
    public String contentType() {
        return contentType;
    }

    /** The tagged object. */
    @Override
    public JsonObject toJson() {
        JsonObject o = new JsonObject();
        o.set(KEY, Base64.getEncoder().encodeToString(data));
        if (filename != null) {
            o.set("filename", filename);
        }
        if (contentType != null) {
            o.set("content_type", contentType);
        }
        return o;
    }

    /** The binary value a tagged object stands for, or null. */
    public static Binary of(Object v) {
        if (!(v instanceof JsonObject)) {
            return v instanceof Binary ? (Binary) v : null;
        }
        JsonObject o = (JsonObject) v;
        if (!o.has(KEY)) {
            return null;
        }
        byte[] bytes = null;
        String name = null;
        String type = null;
        for (String k : o.keys()) {
            Object val = o.get(k);
            switch (k) {
                case KEY:
                    if (!(val instanceof String)) {
                        return null;
                    }
                    try {
                        bytes = Base64.getDecoder().decode((String) val);
                    } catch (IllegalArgumentException e) {
                        return null;
                    }
                    break;
                case "filename":
                    if (val == null) {
                        break;
                    }
                    if (!(val instanceof String)) {
                        return null;
                    }
                    name = (String) val;
                    break;
                case "content_type":
                    if (val == null) {
                        break;
                    }
                    if (!(val instanceof String)) {
                        return null;
                    }
                    type = (String) val;
                    break;
                default:
                    return null;
            }
        }
        return new Binary(bytes, name, type);
    }

    /** Never shows the bytes. */
    @Override
    public String toString() {
        return "Binary(" + data.length + " bytes)";
    }
}
