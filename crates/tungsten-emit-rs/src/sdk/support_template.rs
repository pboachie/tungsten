// SPDX-License-Identifier: AGPL-3.0-only
//! Helpers the generated modules share: serde adapters for presence and
//! base64, discriminated unions, constraint checks that report issues with
//! exact paths, and the validators the descriptors hand to the runtime.
//! Not part of the SDK's API.

use std::fmt::{self, Display};
use std::sync::{Arc, LazyLock};

use regex::Regex;
use serde::de::{DeserializeOwned, Error as _};
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use tungsten_runtime::{
    Category, Diagnostic, Error, Issue, OperationDescriptor, Outcome, Page, Pages, Patch,
    PathSegment, Response, Retryable, Status, Trace, TypedPages, Validation, Validator,
};

/// A field that may be left out, but not `null`.
pub mod opt {
    use super::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        T::deserialize(deserializer).map(Some)
    }
}

/// A field that must be present and may be `null`.
pub mod nullable {
    use super::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        value: &Option<T>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        deserializer: D,
    ) -> Result<Option<T>, D::Error> {
        Option::<T>::deserialize(deserializer)
    }
}

/// Bytes that are base64 text on the wire, where a field's own type cannot
/// carry them (array items, map values, union payloads).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Base64(pub Vec<u8>);

impl From<Vec<u8>> for Base64 {
    fn from(bytes: Vec<u8>) -> Self {
        Base64(bytes)
    }
}

impl Serialize for Base64 {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        tungsten_runtime::b64::serialize(&self.0, serializer)
    }
}

impl<'de> Deserialize<'de> for Base64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        tungsten_runtime::b64::deserialize(deserializer).map(Base64)
    }
}

/// An optional base64 field.
pub mod b64_opt {
    use super::{Base64, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(bytes) => tungsten_runtime::b64::serialize(bytes, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        Base64::deserialize(deserializer).map(|b| Some(b.0))
    }
}

/// A required, nullable base64 field.
pub mod b64_nullable {
    use super::{Base64, Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Option<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(bytes) => tungsten_runtime::b64::serialize(bytes, serializer),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Vec<u8>>, D::Error> {
        Option::<Base64>::deserialize(deserializer).map(|v| v.map(|b| b.0))
    }
}

/// An optional, nullable base64 field.
pub mod b64_patch {
    use super::{Base64, Deserialize, Deserializer, Patch, Serializer};

    pub fn serialize<S: Serializer>(
        value: &Patch<Vec<u8>>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Patch::Value(bytes) => tungsten_runtime::b64::serialize(bytes, serializer),
            Patch::Undefined | Patch::Null => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Patch<Vec<u8>>, D::Error> {
        Ok(match Option::<Base64>::deserialize(deserializer)? {
            Some(b) => Patch::Value(b.0),
            None => Patch::Null,
        })
    }
}

// ------------------------------------------------------------ locations

/// An error message that carries the location of the problem inside a
/// value that serde decoded in pieces (a union's variant).
fn located(path: &serde_path_to_error::Path, message: &str) -> String {
    let (inner, message) = split_located(message);
    let mut segments: Vec<Value> = path
        .iter()
        .filter_map(|s| match s {
            serde_path_to_error::Segment::Seq { index } => Some(Value::from(*index)),
            serde_path_to_error::Segment::Map { key } => Some(Value::from(key.as_str())),
            serde_path_to_error::Segment::Enum { variant } => Some(Value::from(variant.as_str())),
            serde_path_to_error::Segment::Unknown => None,
        })
        .collect();
    segments.extend(inner.iter().map(|s| match s {
        PathSegment::Key(k) => Value::from(k.as_str()),
        PathSegment::Index(i) => Value::from(*i),
    }));
    if segments.is_empty() {
        return message;
    }
    let path = serde_json::to_string(&segments).unwrap_or_default();
    format!("at {path}: {message}")
}

/// The location prefix of a message made by [`located`], and the rest.
fn split_located(message: &str) -> (Vec<PathSegment>, String) {
    let Some(rest) = message.strip_prefix("at [") else {
        return (vec![], message.to_string());
    };
    let rest = format!("[{rest}");
    let mut parsed = serde_json::Deserializer::from_str(&rest).into_iter::<Vec<Value>>();
    let (Some(Ok(items)), used) = (parsed.next(), parsed.byte_offset()) else {
        return (vec![], message.to_string());
    };
    let Some(text) = rest.get(used..).and_then(|t| t.strip_prefix(": ")) else {
        return (vec![], message.to_string());
    };
    let segments = items
        .iter()
        .map(|v| match v {
            Value::Number(n) => PathSegment::Index(n.as_u64().unwrap_or(0) as usize),
            Value::String(k) => PathSegment::Key(k.clone()),
            other => PathSegment::Key(other.to_string()),
        })
        .collect();
    (segments, text.to_string())
}

/// The issue for a serde error at `path`.
pub fn issue_from(path: &serde_path_to_error::Path, message: &str) -> Issue {
    let (inner, message) = split_located(message);
    let mut segments: Vec<PathSegment> = path
        .iter()
        .filter_map(|s| match s {
            serde_path_to_error::Segment::Seq { index } => Some(PathSegment::Index(*index)),
            serde_path_to_error::Segment::Map { key } => Some(PathSegment::Key(key.clone())),
            serde_path_to_error::Segment::Enum { variant } => {
                Some(PathSegment::Key(variant.clone()))
            }
            serde_path_to_error::Segment::Unknown => None,
        })
        .collect();
    segments.extend(inner);
    // serde names a missing field at its parent's path; the path of an
    // unknown field already ends with the field.
    for prefix in ["missing field `", "unknown field `"] {
        if let Some(key) = message
            .strip_prefix(prefix)
            .and_then(|rest| rest.split('`').next())
        {
            let named = matches!(segments.last(), Some(PathSegment::Key(last)) if last == key);
            if prefix.starts_with("missing") || !named {
                segments.push(PathSegment::Key(key.to_string()));
            }
            break;
        }
    }
    Issue {
        path: segments,
        message,
    }
}

// ---------------------------------------------------------------- unions

fn describe(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// Decode a value that serde buffered, keeping the location of an error.
pub fn decode<T: DeserializeOwned, E: serde::de::Error>(value: Value) -> Result<T, E> {
    serde_path_to_error::deserialize(value)
        .map_err(|e| E::custom(located(e.path(), &e.inner().to_string())))
}

/// Try one variant of an untagged union.
pub fn try_decode<T: DeserializeOwned>(value: &Value) -> Option<T> {
    T::deserialize(value).ok()
}

pub fn no_variant<E: serde::de::Error>(name: &str) -> E {
    E::custom(format!("data did not match any variant of `{name}`"))
}

/// Whether `value` is the constant whose JSON text is `json`.
pub fn is_const(value: &Value, json: &str) -> bool {
    serde_json::from_str::<Value>(json).is_ok_and(|c| same(&c, value))
}

pub fn serialize_const<S: Serializer>(serializer: S, json: &str) -> Result<S::Ok, S::Error> {
    serde_json::from_str::<Value>(json)
        .map_err(S::Error::custom)?
        .serialize(serializer)
}

/// A value that no variant of the closed enum `name` has.
pub fn unexpected<E: serde::de::Error>(value: impl Display, name: &str) -> E {
    E::custom(format!("invalid value `{value}` for `{name}`"))
}

/// Serialize a discriminated union's variant, adding the tag when the
/// variant's record has no field for it.
pub fn serialize_tagged<S: Serializer, T: Serialize>(
    serializer: S,
    property: &str,
    tag: &str,
    inner: &T,
) -> Result<S::Ok, S::Error> {
    let value = serde_json::to_value(inner).map_err(S::Error::custom)?;
    match value {
        Value::Object(map) if !map.contains_key(property) => {
            let mut tagged = Map::with_capacity(map.len() + 1);
            tagged.insert(property.to_string(), Value::String(tag.to_string()));
            tagged.extend(map);
            Value::Object(tagged).serialize(serializer)
        }
        other => other.serialize(serializer),
    }
}

/// Read a discriminated union's tag; the value is kept whole.
pub fn read_tagged<'de, D: Deserializer<'de>>(
    deserializer: D,
    property: &str,
) -> Result<(String, Value), D::Error> {
    let value = Value::deserialize(deserializer)?;
    let tag = match &value {
        Value::Object(map) => match map.get(property) {
            Some(Value::String(tag)) => tag.clone(),
            Some(other) => {
                return Err(D::Error::custom(format!(
                    "the discriminator `{property}` must be a string, found {}",
                    describe(other)
                )));
            }
            None => return Err(D::Error::custom(format!("missing field `{property}`"))),
        },
        other => {
            return Err(D::Error::custom(format!(
                "expected an object with a `{property}` member, found {}",
                describe(other)
            )));
        }
    };
    Ok((tag, value))
}

/// Decode the variant a tag selected. `keep` is false when the variant's
/// record has no field for the tag.
pub fn variant<T: DeserializeOwned, E: serde::de::Error>(
    mut value: Value,
    property: &str,
    keep: bool,
) -> Result<T, E> {
    if !keep && let Value::Object(map) = &mut value {
        map.shift_remove(property);
    }
    decode(value)
}

/// Decode the variant a tag selected into the union.
pub fn variant_into<T: DeserializeOwned, U, E: serde::de::Error>(
    value: Value,
    property: &str,
    keep: bool,
    make: fn(T) -> U,
) -> Result<U, E> {
    variant(value, property, keep).map(make)
}

pub fn unknown_tag<E: serde::de::Error>(property: &str, tag: &str, expected: &str) -> E {
    E::custom(format!(
        "unknown value `{tag}` for the discriminator `{property}`, expected one of: {expected}"
    ))
}

// ---------------------------------------------------------------- checks

/// JSON equality: `1` equals `1.0`, `true` is not `1`.
fn same(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            if let (Some(x), Some(y)) = (x.as_i64(), y.as_i64()) {
                x == y
            } else if let (Some(x), Some(y)) = (x.as_u64(), y.as_u64()) {
                x == y
            } else {
                x.as_f64() == y.as_f64()
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(x, y)| same(x, y))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| same(v, w)))
        }
        _ => a == b,
    }
}

/// The string formats the SDK checks, with the rules of the TypeScript and
/// Python SDKs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Uuid,
    Email,
    DateTime,
    Date,
    Ipv4,
    Ipv6,
}

const DATE: &str = concat!(
    "(?:(?:[0-9][0-9][2468][048]|[0-9][0-9][13579][26]|[0-9][0-9]0[48]|[02468][048]00",
    "|[13579][26]00)-02-29|[0-9]{4}-(?:(?:0[13578]|1[02])-(?:0[1-9]|[12][0-9]|3[01])",
    "|(?:0[469]|11)-(?:0[1-9]|[12][0-9]|30)|(?:02)-(?:0[1-9]|1[0-9]|2[0-8])))"
);
const IPV4_PART: &str = "(?:25[0-5]|2[0-4][0-9]|1[0-9][0-9]|[1-9][0-9]|[0-9])";
const IPV6: &str = concat!(
    "(?:(?:H:){7}H|(?:H:){1,7}:|(?:H:){1,6}:H|(?:H:){1,5}(?::H){1,2}",
    "|(?:H:){1,4}(?::H){1,3}|(?:H:){1,3}(?::H){1,4}|(?:H:){1,2}(?::H){1,5}",
    "|H:(?:(?::H){1,6})|:(?:(?::H){1,7}|:))"
);

fn compile(source: &str) -> Option<Regex> {
    Regex::new(&format!("^(?:{source})$")).ok()
}

static UUID: LazyLock<Option<Regex>> = LazyLock::new(|| {
    compile("[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
});
static EMAIL: LazyLock<Option<Regex>> = LazyLock::new(|| compile("[^\\s@\"]{1,64}@[^\\s@]{1,255}"));
static DATE_TIME: LazyLock<Option<Regex>> = LazyLock::new(|| {
    compile(&format!(
        "{DATE}T(?:[01][0-9]|2[0-3]):[0-5][0-9]:[0-5][0-9](?:\\.[0-9]+)?\
         (?:Z|[+-](?:[01][0-9]|2[0-3]):[0-5][0-9])"
    ))
});
static DATE_ONLY: LazyLock<Option<Regex>> = LazyLock::new(|| compile(DATE));
static IPV4: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile(&format!("(?:{IPV4_PART}\\.){{3}}{IPV4_PART}")));
static IPV6_RE: LazyLock<Option<Regex>> =
    LazyLock::new(|| compile(&IPV6.replace('H', "[0-9a-fA-F]{1,4}")));

impl Format {
    fn name(self) -> &'static str {
        match self {
            Format::Uuid => "uuid",
            Format::Email => "email",
            Format::DateTime => "date-time",
            Format::Date => "date",
            Format::Ipv4 => "ipv4",
            Format::Ipv6 => "ipv6",
        }
    }

    fn matches(self, value: &str) -> bool {
        let re = match self {
            Format::Uuid => &UUID,
            Format::Email => &EMAIL,
            Format::DateTime => &DATE_TIME,
            Format::Date => &DATE_ONLY,
            Format::Ipv4 => &IPV4,
            Format::Ipv6 => &IPV6_RE,
        };
        re.as_ref().is_none_or(|r| r.is_match(value))
    }
}

/// Collects the constraint violations of a decoded value, each at the path
/// where it was found.
#[derive(Debug, Default)]
pub struct Checker {
    path: Vec<PathSegment>,
    issues: Vec<Issue>,
}

impl Checker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn finish(self) -> Vec<Issue> {
        self.issues
    }

    pub fn push_key(&mut self, key: &str) {
        self.path.push(PathSegment::Key(key.to_string()));
    }

    pub fn push_index(&mut self, index: usize) {
        self.path.push(PathSegment::Index(index));
    }

    pub fn pop(&mut self) {
        self.path.pop();
    }

    pub fn issue(&mut self, message: impl Into<String>) {
        self.issues.push(Issue {
            path: self.path.clone(),
            message: message.into(),
        });
    }

    pub fn str_len(&mut self, value: &str, min: Option<u64>, max: Option<u64>) {
        let len = value.chars().count() as u64;
        if let Some(min) = min.filter(|m| len < *m) {
            self.issue(format!("must have at least {min} characters"));
        }
        if let Some(max) = max.filter(|m| len > *m) {
            self.issue(format!("must have at most {max} characters"));
        }
    }

    pub fn items(&mut self, len: usize, min: Option<u64>, max: Option<u64>) {
        let len = len as u64;
        if let Some(min) = min.filter(|m| len < *m) {
            self.issue(format!("must have at least {min} items"));
        }
        if let Some(max) = max.filter(|m| len > *m) {
            self.issue(format!("must have at most {max} items"));
        }
    }

    pub fn unique<T: Serialize>(&mut self, items: &[T]) {
        let values: Vec<Value> = items
            .iter()
            .map(|i| serde_json::to_value(i).unwrap_or(Value::Null))
            .collect();
        let duplicate = values
            .iter()
            .enumerate()
            .any(|(i, a)| values[..i].iter().any(|b| same(a, b)));
        if duplicate {
            self.issue("items must be unique");
        }
    }

    pub fn pattern(&mut self, value: &str, re: &LazyLock<Option<Regex>>, source: &str) {
        if re.as_ref().is_some_and(|r| !r.is_match(value)) {
            self.issue(format!("must match the pattern /{source}/"));
        }
    }

    pub fn format(&mut self, value: &str, format: Format) {
        if !format.matches(value) {
            self.issue(format!("must be a valid {}", format.name()));
        }
    }

    pub fn min_i(&mut self, value: i128, bound: i128) {
        if value < bound {
            self.issue(format!("must be at least {bound}"));
        }
    }

    pub fn max_i(&mut self, value: i128, bound: i128) {
        if value > bound {
            self.issue(format!("must be at most {bound}"));
        }
    }

    pub fn gt_i(&mut self, value: i128, bound: i128) {
        if value <= bound {
            self.issue(format!("must be greater than {bound}"));
        }
    }

    pub fn lt_i(&mut self, value: i128, bound: i128) {
        if value >= bound {
            self.issue(format!("must be less than {bound}"));
        }
    }

    pub fn multiple_i(&mut self, value: i128, step: i128) {
        if step != 0 && value % step != 0 {
            self.issue(format!("must be a multiple of {step}"));
        }
    }

    pub fn min_f(&mut self, value: f64, bound: f64) {
        if value < bound {
            self.issue(format!("must be at least {bound}"));
        }
    }

    pub fn max_f(&mut self, value: f64, bound: f64) {
        if value > bound {
            self.issue(format!("must be at most {bound}"));
        }
    }

    pub fn gt_f(&mut self, value: f64, bound: f64) {
        if value <= bound {
            self.issue(format!("must be greater than {bound}"));
        }
    }

    pub fn lt_f(&mut self, value: f64, bound: f64) {
        if value >= bound {
            self.issue(format!("must be less than {bound}"));
        }
    }

    pub fn multiple_f(&mut self, value: f64, step: f64) {
        let quotient = value / step;
        if step != 0.0 && (quotient - quotient.round()).abs() > 1e-9 * quotient.abs().max(1.0) {
            self.issue(format!("must be a multiple of {step}"));
        }
    }

    pub fn const_str(&mut self, value: &str, expected: &str) {
        if value != expected {
            self.issue(format!("must be {expected:?}"));
        }
    }

    pub fn const_bool(&mut self, value: bool, expected: bool) {
        if value != expected {
            self.issue(format!("must be {expected}"));
        }
    }

    pub fn const_int(&mut self, value: i64, expected: i64) {
        if value != expected {
            self.issue(format!("must be {expected}"));
        }
    }

    pub fn const_json(&mut self, value: &Value, expected: &str) {
        if !is_const(value, expected) {
            self.issue(format!("must be {expected}"));
        }
    }

    /// The value must be one of the JSON values in `allowed` (a JSON array).
    pub fn one_of(&mut self, value: &Value, allowed: &str) {
        let list: Vec<Value> = serde_json::from_str(allowed).unwrap_or_default();
        if !list.iter().any(|a| same(a, value)) {
            self.issue(format!("must be one of {allowed}"));
        }
    }

    /// The value must also decode as `T` (a member of an `allOf`) and pass
    /// its checks.
    pub fn member<T: DeserializeOwned>(&mut self, value: &Value, check: fn(&T, &mut Checker)) {
        match serde_path_to_error::deserialize::<_, T>(value) {
            Ok(decoded) => check(&decoded, self),
            Err(e) => {
                let issue = issue_from(e.path(), &e.inner().to_string());
                let mut path = self.path.clone();
                path.extend(issue.path);
                self.issues.push(Issue {
                    path,
                    message: issue.message,
                });
            }
        }
    }
}

/// A type without constraints.
pub fn no_check<T>(_value: &T, _c: &mut Checker) {}

// ------------------------------------------------------------ validators

/// Validates a value by decoding it as `T` and running `T`'s checks.
pub struct Typed<T> {
    check: fn(&T, &mut Checker),
    request: bool,
}

impl<T> Typed<T> {
    /// Validates an arguments object; the valid value is the normalized
    /// arguments (absent optionals left out).
    pub const fn request(check: fn(&T, &mut Checker)) -> Self {
        Typed {
            check,
            request: true,
        }
    }

    /// Judges a body or a page item.
    pub const fn body(check: fn(&T, &mut Checker)) -> Self {
        Typed {
            check,
            request: false,
        }
    }
}

impl<T> fmt::Debug for Typed<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Typed")
            .field("type", &std::any::type_name::<T>())
            .field("request", &self.request)
            .finish()
    }
}

/// Remove the member at `path` of `value`; false when there is none.
fn remove_member(value: &mut Value, path: &[PathSegment]) -> bool {
    let Some((PathSegment::Key(last), parents)) = path.split_last() else {
        return false;
    };
    let mut node = value;
    for segment in parents {
        let next = match (segment, node) {
            (PathSegment::Key(key), Value::Object(map)) => map.get_mut(key),
            (PathSegment::Index(index), Value::Array(items)) => items.get_mut(*index),
            _ => None,
        };
        let Some(next) = next else {
            return false;
        };
        node = next;
    }
    node.as_object_mut()
        .is_some_and(|map| map.remove(last).is_some())
}

impl<T: DeserializeOwned + Serialize + 'static> Validator for Typed<T> {
    /// Issues come in the order of the other runtimes' schema libraries:
    /// missing and invalid members and constraint violations first, members
    /// the schema does not allow last (serde stops at an unknown member when
    /// it meets it, so it is set aside and the rest is judged first).
    fn validate(&self, value: &Value) -> Validation {
        let mut rest: Option<Value> = None;
        let mut unknown: Vec<Issue> = Vec::new();
        let decoded = loop {
            let current = rest.as_ref().unwrap_or(value);
            match serde_path_to_error::deserialize::<_, T>(current) {
                Ok(decoded) => break decoded,
                Err(e) => {
                    let message = e.inner().to_string();
                    let issue = issue_from(e.path(), &message);
                    if message.starts_with("unknown field `")
                        && remove_member(rest.get_or_insert_with(|| value.clone()), &issue.path)
                    {
                        unknown.push(issue);
                        continue;
                    }
                    let mut issues = vec![issue];
                    issues.append(&mut unknown);
                    return Validation::Invalid(issues);
                }
            }
        };
        let mut checker = Checker::new();
        (self.check)(&decoded, &mut checker);
        let mut issues = checker.finish();
        issues.append(&mut unknown);
        if !issues.is_empty() {
            return Validation::Invalid(issues);
        }
        if !self.request {
            return Validation::Valid(value.clone());
        }
        match serde_json::to_value(&decoded) {
            Ok(normalized) => Validation::Valid(normalized),
            Err(e) => Validation::Invalid(vec![Issue {
                path: vec![],
                message: e.to_string(),
            }]),
        }
    }
}

// ----------------------------------------------------------- descriptors

/// An owned string (descriptor data).
pub fn s(text: &str) -> String {
    text.to_string()
}

/// A JSON value from its text (descriptor data).
pub fn json(text: &str) -> Value {
    serde_json::from_str(text).unwrap_or(Value::Null)
}

// -------------------------------------------------------------- dispatch

/// A request struct as the arguments object.
pub fn args<T: Serialize>(request: &T) -> Value {
    serde_json::to_value(request).unwrap_or(Value::Null)
}

/// The envelope for a call that cannot be dispatched.
pub fn malformed(
    operation: &str,
    parameter: &str,
    received: Value,
    expected: &str,
    remediation: String,
) -> Error {
    Error::new(Diagnostic {
        status: Status::Error,
        category: Category::MalformedRequest,
        operation: operation.to_string(),
        http_status: None,
        code: None,
        failed_parameter: Some(parameter.to_string()),
        received_value: received,
        expected: Some(expected.to_string()),
        remediation,
        retryable: Retryable::Never,
        retry_after_ms: None,
        next_action: None,
        request_id: None,
        trace: Trace { attempts: 0 },
    })
}

/// A typed outcome as a dynamic one: the value as JSON, `None` when it is
/// `null` (a response without a body).
pub fn encode<T: Serialize>(result: tungsten_runtime::Result<T>) -> Outcome {
    let response = result?;
    let value = match serde_json::to_value(&response.value) {
        Ok(Value::Null) => None,
        Ok(value) => Some(value),
        Err(e) => {
            return Err(malformed(
                "response",
                "response",
                Value::Null,
                "a value that serializes to JSON",
                e.to_string(),
            ));
        }
    };
    Ok(Response {
        value,
        meta: response.meta,
        verification: response.verification,
    })
}

/// A typed page as a dynamic one.
pub fn encode_page<T: Serialize>(
    result: tungsten_runtime::Result<Page<T>>,
) -> tungsten_runtime::Result<Page<Value>> {
    let response = result?;
    let page = response.value;
    let items = page
        .items
        .iter()
        .map(|i| serde_json::to_value(i).unwrap_or(Value::Null))
        .collect();
    Ok(Response {
        value: Page {
            items,
            body: page.body,
            next: page.next,
        },
        meta: response.meta,
        verification: response.verification,
    })
}

/// The operation with this id.
pub fn find<'a>(
    operations: &'a [Arc<OperationDescriptor>],
    id: &str,
) -> Option<&'a Arc<OperationDescriptor>> {
    operations.iter().find(|op| op.id == id)
}

/// Arguments that decode as a request struct, if they do.
pub fn decode_args<T: DeserializeOwned>(args: &Value) -> Option<T> {
    T::deserialize(args).ok()
}

pub fn unknown_operation(operation: &str) -> Error {
    malformed(
        operation,
        "operation",
        Value::String(operation.to_string()),
        "an operation id of this API",
        format!(
            "`{operation}` is not an operation of this API; list the operations with `Dispatch::operations`."
        ),
    )
}

pub fn unknown_macro(name: &str) -> Error {
    malformed(
        name,
        "macro",
        Value::String(name.to_string()),
        "a macro name of this API",
        format!("`{name}` is not a macro of this API; list the macros with `Dispatch::macros`."),
    )
}

pub fn not_paginated(operation: &str) -> Error {
    malformed(
        operation,
        "operation",
        Value::String(operation.to_string()),
        "the id of a paginated operation",
        format!("`{operation}` is not a paginated operation of this API."),
    )
}

/// Every page of a typed iteration, as dynamic results.
pub async fn collect_typed<T: Serialize + DeserializeOwned>(
    mut pages: TypedPages<T>,
) -> Vec<tungsten_runtime::Result<Page<Value>>> {
    let mut out = vec![];
    while let Some(page) = pages.next().await {
        out.push(encode_page(page));
    }
    out
}

/// Every page of an untyped iteration.
pub async fn collect_raw(mut pages: Pages) -> Vec<tungsten_runtime::Result<Page<Value>>> {
    let mut out = vec![];
    while let Some(page) = pages.next().await {
        out.push(page);
    }
    out
}
