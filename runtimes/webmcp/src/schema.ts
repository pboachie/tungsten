// SPDX-License-Identifier: Apache-2.0
/** Browser hosts do not enforce inputSchema. Validate the generated schema's
 * structural constraints here, before ClientCore's full descriptor validation.
 * Formats are annotations; runtime argument schemas enforce their semantics. */
export function matchesSchema(value: unknown, root: Record<string, unknown>): boolean {
  let budget = 100000;
  const object = (v: unknown): v is Record<string, unknown> => typeof v === "object" && v !== null && !Array.isArray(v);
  const equal = (a: unknown, b: unknown): boolean => {
    if (a === b) return true;
    if (Array.isArray(a) && Array.isArray(b)) return a.length === b.length && a.every((v, i) => equal(v, b[i]));
    if (object(a) && object(b)) {
      const keys = Object.keys(a);
      return keys.length === Object.keys(b).length && keys.every((key) => Object.hasOwn(b, key) && equal(a[key], b[key]));
    }
    return false;
  };
  const type = (v: unknown, t: unknown): boolean => {
    switch (t) {
      case "null": return v === null;
      case "object": return object(v);
      case "array": return Array.isArray(v);
      case "integer": return typeof v === "number" && Number.isInteger(v);
      case "number": return typeof v === "number" && Number.isFinite(v);
      case "boolean": return typeof v === "boolean";
      case "string": return typeof v === "string";
      default: return false;
    }
  };
  const check = (v: unknown, s: unknown, depth: number): boolean => {
    if (--budget < 0 || depth > 100) return false;
    if (typeof s === "boolean") return s;
    if (!object(s)) return false;
    if (typeof s.$ref === "string") {
      if (!s.$ref.startsWith("#/")) return false;
      let target: unknown = root;
      for (const part of s.$ref.slice(2).split("/")) {
        const key = part.replace(/~1/g, "/").replace(/~0/g, "~");
        if (!object(target) || !Object.hasOwn(target, key)) return false;
        target = target[key];
      }
      if (!check(v, target, depth + 1)) return false;
    }
    if (Object.hasOwn(s, "const") && !equal(v, s.const)) return false;
    if (Array.isArray(s.enum) && !s.enum.some((member) => equal(v, member))) return false;
    if (s.type !== undefined && !(Array.isArray(s.type) ? s.type.some((t) => type(v, t)) : type(v, s.type))) return false;
    if (Array.isArray(s.allOf) && !s.allOf.every((branch) => check(v, branch, depth + 1))) return false;
    if (Array.isArray(s.anyOf) && !s.anyOf.some((branch) => check(v, branch, depth + 1))) return false;
    if (Array.isArray(s.oneOf) && s.oneOf.filter((branch) => check(v, branch, depth + 1)).length !== 1) return false;
    if (s.not !== undefined && check(v, s.not, depth + 1)) return false;
    if (s.if !== undefined && !check(v, check(v, s.if, depth + 1) ? s.then ?? true : s.else ?? true, depth + 1)) return false;
    if (typeof v === "number") {
      if (typeof s.minimum === "number" && v < s.minimum || typeof s.maximum === "number" && v > s.maximum || typeof s.exclusiveMinimum === "number" && v <= s.exclusiveMinimum || typeof s.exclusiveMaximum === "number" && v >= s.exclusiveMaximum) return false;
      if (typeof s.multipleOf === "number" && (!Number.isFinite(v / s.multipleOf) || Math.abs(v / s.multipleOf - Math.round(v / s.multipleOf)) > 1e-10)) return false;
    }
    if (typeof v === "string") {
      const length = [...v].length;
      if (typeof s.minLength === "number" && length < s.minLength || typeof s.maxLength === "number" && length > s.maxLength) return false;
      if (typeof s.pattern === "string" && !new RegExp(s.pattern, "u").test(v)) return false;
    }
    if (Array.isArray(v)) {
      if (typeof s.minItems === "number" && v.length < s.minItems || typeof s.maxItems === "number" && v.length > s.maxItems) return false;
      if (s.uniqueItems === true && v.some((member, i) => v.slice(0, i).some((previous) => equal(member, previous)))) return false;
      const prefix = Array.isArray(s.prefixItems) ? s.prefixItems : [];
      if (!v.every((item, i) => check(item, prefix[i] ?? s.items ?? true, depth + 1))) return false;
      if (s.contains !== undefined) {
        const count = v.filter((item) => check(item, s.contains, depth + 1)).length;
        if (count < (typeof s.minContains === "number" ? s.minContains : 1) || typeof s.maxContains === "number" && count > s.maxContains) return false;
      }
    }
    if (object(v)) {
      const keys = Object.keys(v);
      if (Array.isArray(s.required) && s.required.some((key) => typeof key !== "string" || !Object.hasOwn(v, key))) return false;
      if (typeof s.minProperties === "number" && keys.length < s.minProperties || typeof s.maxProperties === "number" && keys.length > s.maxProperties) return false;
      const properties = object(s.properties) ? s.properties : {};
      const patterns = object(s.patternProperties) ? s.patternProperties : {};
      for (const key of keys) {
        let known = Object.hasOwn(properties, key);
        if (known && !check(v[key], properties[key], depth + 1)) return false;
        for (const [pattern, schema] of Object.entries(patterns)) if (new RegExp(pattern, "u").test(key)) {
          known = true;
          if (!check(v[key], schema, depth + 1)) return false;
        }
        if (!known && !check(v[key], s.additionalProperties ?? true, depth + 1)) return false;
        if (s.propertyNames !== undefined && !check(key, s.propertyNames, depth + 1)) return false;
      }
      if (object(s.dependentRequired)) for (const [key, deps] of Object.entries(s.dependentRequired)) {
        if (Object.hasOwn(v, key) && Array.isArray(deps) && deps.some((dep) => typeof dep !== "string" || !Object.hasOwn(v, dep))) return false;
      }
      if (object(s.dependentSchemas)) for (const [key, schema] of Object.entries(s.dependentSchemas)) {
        if (Object.hasOwn(v, key) && !check(v, schema, depth + 1)) return false;
      }
    }
    return true;
  };
  return check(value, root, 0);
}
