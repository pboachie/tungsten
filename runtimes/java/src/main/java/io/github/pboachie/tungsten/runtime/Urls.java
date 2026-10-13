// SPDX-License-Identifier: Apache-2.0
package io.github.pboachie.tungsten.runtime;

import java.net.URI;
import java.net.URISyntaxException;
import java.util.ArrayList;
import java.util.List;
import java.util.Locale;

/** URL helpers: resolution, origins and query parameters (URLSearchParams semantics). */
final class Urls {
    private Urls() {}

    /** The location resolved against the base URL, or null. */
    static URI resolve(String base, String location) {
        try {
            return new URI(base).resolve(new URI(location));
        } catch (URISyntaxException | IllegalArgumentException e) {
            return null;
        }
    }

    private static int port(URI u) {
        if (u.getPort() >= 0) {
            return u.getPort();
        }
        String scheme = u.getScheme() == null ? "" : u.getScheme().toLowerCase(Locale.ROOT);
        return scheme.equals("http") ? 80 : scheme.equals("https") ? 443 : -1;
    }

    static boolean sameOrigin(URI a, URI b) {
        return a.getScheme() != null && b.getScheme() != null
                && a.getScheme().equalsIgnoreCase(b.getScheme())
                && a.getHost() != null && b.getHost() != null
                && a.getHost().equalsIgnoreCase(b.getHost())
                && port(a) == port(b);
    }

    private static String query(String url) {
        int q = url.indexOf('?');
        if (q < 0) {
            return null;
        }
        int h = url.indexOf('#', q);
        return h < 0 ? url.substring(q + 1) : url.substring(q + 1, h);
    }

    private static List<Serialize.Pair> pairs(String url) {
        List<Serialize.Pair> out = new ArrayList<>();
        String q = query(url);
        if (q == null || q.isEmpty()) {
            return out;
        }
        for (String part : q.split("&", -1)) {
            if (part.isEmpty()) {
                continue;
            }
            int eq = part.indexOf('=');
            String k = eq < 0 ? part : part.substring(0, eq);
            String v = eq < 0 ? "" : part.substring(eq + 1);
            out.add(new Serialize.Pair(Text.decodeQueryComponent(k), Text.decodeQueryComponent(v)));
        }
        return out;
    }

    static boolean hasQuery(String url, String name) {
        for (Serialize.Pair p : pairs(url)) {
            if (p.k().equals(name)) {
                return true;
            }
        }
        return false;
    }

    /** The URL with the query parameter set, replacing existing ones (URLSearchParams.set). */
    static String setQuery(String url, String name, String value) {
        List<Serialize.Pair> list = pairs(url);
        List<Serialize.Pair> kept = new ArrayList<>();
        boolean seen = false;
        for (Serialize.Pair p : list) {
            if (p.k().equals(name)) {
                if (!seen) {
                    kept.add(new Serialize.Pair(name, value));
                    seen = true;
                }
            } else {
                kept.add(p);
            }
        }
        if (!seen) {
            kept.add(new Serialize.Pair(name, value));
        }
        List<String> parts = new ArrayList<>();
        for (Serialize.Pair p : kept) {
            parts.add(Text.encodeFormComponent(p.k()) + "=" + Text.encodeFormComponent(p.v()));
        }
        int q = url.indexOf('?');
        int h = url.indexOf('#');
        String head = q >= 0 ? url.substring(0, q) : (h >= 0 ? url.substring(0, h) : url);
        String fragment = h >= 0 ? url.substring(h) : "";
        return head + "?" + String.join("&", parts) + fragment;
    }
}
