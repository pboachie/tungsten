// SPDX-License-Identifier: AGPL-3.0-only
//! Deterministic values from IR types. Every choice derives from a hash of
//! the seed, the scope (an operation id) and the field path, so the same
//! inputs always produce the same bytes.

use std::cell::Cell;

use serde_json::{Map, Value};
use tungsten_ir::{
    Constraints, Field, Presence, Primitive, Shape, StringFormat, TypeRef, Union, UnionStrategy,
    Variant,
};

use crate::model::Model;
use crate::pattern::{self, Rng};
use crate::validate::{Context, Validator};

/// Beyond this depth optional fields are left out, arrays and maps take
/// their minimum size and nullables are null, so recursive types end.
const SHALLOW_DEPTH: usize = 8;
/// Beyond this depth the value is `null`.
const MAX_DEPTH: usize = 64;
/// Type nodes visited per value before the rest becomes `null`.
const MAX_STEPS: usize = 100_000;
/// Most items generated for an array with a large `minItems`.
const MAX_ITEMS: u64 = 256;
/// 2026-01-01T00:00:00Z, the base of generated timestamps.
const EPOCH_2026: i64 = 1_767_225_600;

#[derive(Debug)]
pub(crate) struct Generator<'a> {
    model: &'a Model,
    ctx: Context,
    steps: Cell<usize>,
}

impl<'a> Generator<'a> {
    pub fn new(model: &'a Model, ctx: Context) -> Generator<'a> {
        Generator {
            model,
            ctx,
            steps: Cell::new(0),
        }
    }

    /// The value for `ty` under `scope` (an operation id) at `path`.
    pub fn value(&self, ty: &TypeRef, scope: &str, path: &str) -> Value {
        self.steps.set(0);
        let mut path = path.to_string();
        self.ty(ty, scope, &mut path, "", 0, None)
    }

    fn ty(
        &self,
        ty: &TypeRef,
        scope: &str,
        path: &mut String,
        name: &str,
        depth: usize,
        extra: Option<&Constraints>,
    ) -> Value {
        let steps = self.steps.get() + 1;
        self.steps.set(steps);
        if depth > MAX_DEPTH || steps > MAX_STEPS {
            return Value::Null;
        }
        match self.model.shape(ty) {
            Some(shape) => self.shape(shape, scope, path, name, depth, extra),
            None => Value::Null,
        }
    }

    fn shape(
        &self,
        shape: &Shape,
        scope: &str,
        path: &mut String,
        name: &str,
        depth: usize,
        extra: Option<&Constraints>,
    ) -> Value {
        let shallow = depth >= SHALLOW_DEPTH;
        match shape {
            Shape::Primitive {
                primitive,
                constraints,
            } => {
                let merged = merge(constraints, extra);
                let h = hash(self.model.seed, scope, path);
                self.primitive(primitive, &merged, h, name)
            }
            Shape::Enum { values, .. } => values
                .first()
                .map(|v| v.value.clone())
                .unwrap_or(Value::Null),
            Shape::Const { value } => value.clone(),
            Shape::Array {
                items, min, max, ..
            } => {
                let floor = min.unwrap_or(0);
                let wanted = if shallow { floor } else { floor.max(1) };
                let count = wanted.min(max.unwrap_or(u64::MAX)).min(MAX_ITEMS);
                let mut out = Vec::new();
                for i in 0..count {
                    let mark = path.len();
                    path.push_str(&format!("/{i}"));
                    out.push(self.ty(items, scope, path, name, depth + 1, None));
                    path.truncate(mark);
                }
                Value::Array(out)
            }
            Shape::Map { values } => {
                let mut out = Map::new();
                if !shallow {
                    let mark = path.len();
                    path.push_str("/key");
                    out.insert(
                        "key".into(),
                        self.ty(values, scope, path, name, depth + 1, None),
                    );
                    path.truncate(mark);
                }
                Value::Object(out)
            }
            Shape::Record { fields, .. } => {
                let mut out = Map::new();
                for field in fields {
                    if !self.include(field, shallow) {
                        continue;
                    }
                    let mark = path.len();
                    path.push('/');
                    path.push_str(&field.wire_name);
                    let value = match self.usable_default(field) {
                        Some(default) => default,
                        None => self.ty(
                            &field.ty,
                            scope,
                            path,
                            &field.wire_name,
                            depth + 1,
                            Some(&field.constraints),
                        ),
                    };
                    path.truncate(mark);
                    out.insert(field.wire_name.clone(), value);
                }
                Value::Object(out)
            }
            Shape::Union(union) => self.union(union, scope, path, name, depth),
            Shape::Intersection { members } => {
                let mut merged = Map::new();
                let mut first = None;
                for member in members {
                    match self.ty(member, scope, path, name, depth + 1, None) {
                        Value::Object(map) => {
                            for (k, v) in map {
                                merged.entry(k).or_insert(v);
                            }
                        }
                        other => {
                            first.get_or_insert(other);
                        }
                    }
                }
                match first {
                    Some(value) if merged.is_empty() => value,
                    _ => Value::Object(merged),
                }
            }
            Shape::Nullable { inner } => {
                if shallow {
                    Value::Null
                } else {
                    self.ty(inner, scope, path, name, depth + 1, extra)
                }
            }
            Shape::Any => Value::Object(Map::new()),
            Shape::Never => Value::Null,
        }
    }

    fn include(&self, field: &Field, shallow: bool) -> bool {
        let required = matches!(
            field.presence,
            Presence::Required | Presence::RequiredNullable
        );
        match self.ctx {
            Context::Response if field.write_only => false,
            Context::Request if field.read_only => false,
            _ => required || !shallow,
        }
    }

    /// A field's declared default, when it is a non-null value of its type.
    fn usable_default(&self, field: &Field) -> Option<Value> {
        let default = field.default.as_ref().filter(|d| !d.is_null())?;
        Validator::new(self.model, self.ctx)
            .check(&field.ty, default)
            .ok()
            .map(|()| default.clone())
    }

    /// The events of a stream of `ty` under `scope` at `path`: one for each
    /// variant of a union, in order, else two values of the type. Each is
    /// paired with the discriminator value that tags it, if any.
    pub fn events(&self, ty: &TypeRef, scope: &str, path: &str) -> Vec<(Option<String>, Value)> {
        self.steps.set(0);
        if let Some(Shape::Union(union)) = self.model.shape(ty) {
            return union
                .variants
                .iter()
                .enumerate()
                .map(|(i, variant)| {
                    let mut at = format!("{path}/{i}");
                    let value = self.variant(union, variant, scope, &mut at, "", 0);
                    (tag_of(union, variant), value)
                })
                .collect();
        }
        (0..2)
            .map(|i| {
                let mut at = format!("{path}/{i}");
                (None, self.ty(ty, scope, &mut at, "", 0, None))
            })
            .collect()
    }

    fn union(
        &self,
        union: &Union,
        scope: &str,
        path: &mut String,
        name: &str,
        depth: usize,
    ) -> Value {
        match union.variants.first() {
            Some(variant) => self.variant(union, variant, scope, path, name, depth),
            None => Value::Null,
        }
    }

    /// A value of one variant of a union, with its discriminator set.
    fn variant(
        &self,
        union: &Union,
        variant: &Variant,
        scope: &str,
        path: &mut String,
        name: &str,
        depth: usize,
    ) -> Value {
        let mut value = self.ty(&variant.ty, scope, path, name, depth + 1, None);
        if union.strategy == UnionStrategy::Tagged
            && let Some(discriminator) = &union.discriminator
            && let (Some(tag), Some(object)) = (tag_of(union, variant), value.as_object_mut())
        {
            let consistent = match object.get(&discriminator.property) {
                Some(Value::Null) | None => false,
                Some(present) => tag_text(present) == tag,
            };
            if !consistent {
                object.insert(discriminator.property.clone(), Value::String(tag));
            }
        }
        value
    }

    fn primitive(&self, primitive: &Primitive, c: &Constraints, h: u64, name: &str) -> Value {
        match primitive {
            Primitive::String { format } => Value::String(self.string(format.as_ref(), c, h, name)),
            Primitive::Int32 => {
                Value::from(integer(c, h, name, i32::MIN as i128, i32::MAX as i128))
            }
            Primitive::Int64 | Primitive::Integer => {
                Value::from(integer(c, h, name, i64::MIN as i128, i64::MAX as i128))
            }
            Primitive::Float | Primitive::Double | Primitive::Number => number(c, h, name),
            Primitive::Bool => Value::Bool(true),
            Primitive::Bytes => Value::String(fit(String::new(), c)),
        }
    }

    fn string(&self, format: Option<&StringFormat>, c: &Constraints, h: u64, name: &str) -> String {
        let matches_pattern = |s: &str| {
            c.pattern
                .as_ref()
                .is_none_or(|p| self.model.pattern_matches(p, s))
        };
        let fits = |s: &str| {
            let len = s.chars().count() as u64;
            c.min_length.is_none_or(|m| len >= m) && c.max_length.is_none_or(|m| len <= m)
        };
        if let Some(candidate) = format.and_then(|f| formatted(f, h, name))
            && matches_pattern(&candidate)
        {
            return candidate;
        }
        if let Some(p) = &c.pattern {
            for extra in [0, 1, 4, 16] {
                if let Some(s) = pattern::sample(p, h, extra)
                    && fits(&s)
                    && matches_pattern(&s)
                {
                    return s;
                }
            }
        }
        if format.is_none()
            && c.pattern.is_none()
            && let Some(candidate) = name_format(name).and_then(|f| formatted(&f, h, name))
            && fits(&candidate)
        {
            return candidate;
        }
        fit(format!("{}_{:04x}", slug(name, "value"), h & 0xffff), c)
    }
}

/// A discriminator value as the tag text the IR records.
/// The discriminator value that selects a variant of a tagged union.
fn tag_of(union: &Union, variant: &Variant) -> Option<String> {
    let discriminator = union.discriminator.as_ref()?;
    if union.strategy != UnionStrategy::Tagged {
        return None;
    }
    variant.tag.clone().or_else(|| match &variant.ty {
        TypeRef::Named(id) => discriminator
            .mapping
            .iter()
            .find(|(_, target)| target == id)
            .map(|(k, _)| k.clone()),
        TypeRef::Inline(_) => None,
    })
}

pub(crate) fn tag_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// Field-level constraints override the type's.
fn merge(base: &Constraints, extra: Option<&Constraints>) -> Constraints {
    let Some(extra) = extra else {
        return base.clone();
    };
    Constraints {
        pattern: extra.pattern.clone().or_else(|| base.pattern.clone()),
        min_length: extra.min_length.or(base.min_length),
        max_length: extra.max_length.or(base.max_length),
        minimum: extra.minimum.clone().or_else(|| base.minimum.clone()),
        maximum: extra.maximum.clone().or_else(|| base.maximum.clone()),
        exclusive_minimum: extra
            .exclusive_minimum
            .clone()
            .or_else(|| base.exclusive_minimum.clone()),
        exclusive_maximum: extra
            .exclusive_maximum
            .clone()
            .or_else(|| base.exclusive_maximum.clone()),
        multiple_of: extra
            .multiple_of
            .clone()
            .or_else(|| base.multiple_of.clone()),
    }
}

/// FNV-1a over the seed, scope and path, finished with splitmix64.
pub(crate) fn hash(seed: u64, scope: &str, path: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let bytes = seed
        .to_le_bytes()
        .into_iter()
        .chain([0xff])
        .chain(scope.bytes())
        .chain([0xff])
        .chain(path.bytes());
    for b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Rng(h).next()
}

/// Lowercase ASCII letters, digits and `_` of a name, or `fallback`.
fn slug(name: &str, fallback: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .take(40)
        .collect::<String>()
        .to_ascii_lowercase();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

/// The format a plain string field's name suggests (`callback_url`,
/// `contact_email`).
fn name_format(name: &str) -> Option<StringFormat> {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with("url") || lower.ends_with("uri") {
        Some(StringFormat::Uri)
    } else if lower.ends_with("email") {
        Some(StringFormat::Email)
    } else {
        None
    }
}

/// Pad with `x` or truncate to the length limits.
fn fit(mut s: String, c: &Constraints) -> String {
    let len = s.chars().count() as u64;
    if let Some(min) = c.min_length
        && len < min
    {
        s.extend(std::iter::repeat_n('x', (min - len).min(1 << 16) as usize));
    }
    if let Some(max) = c.max_length
        && (s.chars().count() as u64) > max
    {
        s = s.chars().take(max as usize).collect();
    }
    s
}

fn formatted(format: &StringFormat, h: u64, name: &str) -> Option<String> {
    let seconds = EPOCH_2026 + (h % 31_536_000) as i64;
    Some(match format {
        StringFormat::Uuid => uuid(h),
        StringFormat::DateTime => {
            let (date, time) = civil(seconds);
            format!("{date}T{time}Z")
        }
        StringFormat::Date => civil(seconds).0,
        StringFormat::Time => format!("{}Z", civil(seconds).1),
        StringFormat::Duration => format!("PT{}S", 1 + h % 3600),
        StringFormat::Email => format!("{}@example.test", slug(name, "user")),
        StringFormat::Uri => format!("https://example.test/{}", slug(name, "resource")),
        StringFormat::Hostname => format!("{}.example.test", slug(name, "host").replace('_', "-")),
        StringFormat::Ipv4 => format!("192.0.2.{}", 1 + h % 254),
        StringFormat::Ipv6 => format!("2001:db8::{:x}", 1 + h % 0xfffe),
        StringFormat::Byte => "dHVuZ3N0ZW4=".to_string(),
        StringFormat::Password => format!("secret-{:06x}", h & 0xff_ffff),
        StringFormat::Other(_) => return None,
    })
}

/// A version 4, variant 1 UUID from a hash.
fn uuid(h: u64) -> String {
    let low = Rng(h ^ 0x5555_5555_5555_5555).next();
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        h >> 32,
        (h >> 16) & 0xffff,
        0x4000 | (h & 0x0fff),
        0x8000 | ((low >> 48) & 0x3fff),
        low & 0xffff_ffff_ffff
    )
}

/// `YYYY-MM-DD` and `HH:MM:SS` of a Unix time (UTC).
fn civil(seconds: i64) -> (String, String) {
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    // Howard Hinnant's days-to-civil algorithm.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (
        format!("{year:04}-{month:02}-{day:02}"),
        format!("{:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60),
    )
}

/// An integer within the bounds and multiple; timestamps for names ending
/// in `_ms` or `_at` when unbounded.
fn integer(c: &Constraints, h: u64, name: &str, type_low: i128, type_high: i128) -> i64 {
    let num = |n: &Option<serde_json::Number>| n.as_ref().and_then(serde_json::Number::as_f64);
    let mut low = num(&c.minimum).map(|x| x.ceil() as i128);
    if let Some(x) = num(&c.exclusive_minimum) {
        let bound = x.floor() as i128 + 1;
        low = Some(low.map_or(bound, |l| l.max(bound)));
    }
    let mut high = num(&c.maximum).map(|x| x.floor() as i128);
    if let Some(x) = num(&c.exclusive_maximum) {
        let bound = x.ceil() as i128 - 1;
        high = Some(high.map_or(bound, |hi| hi.min(bound)));
    }
    let low = low.unwrap_or(type_low).max(type_low);
    let high = high.unwrap_or(type_high).min(type_high);
    let preferred: i128 = if name.ends_with("_ms") {
        (EPOCH_2026 as i128) * 1000 + (h % 86_400_000) as i128
    } else if name.ends_with("_at") || name.ends_with("timestamp") {
        EPOCH_2026 as i128 + (h % 86_400) as i128
    } else {
        1 + (h % 100) as i128
    };
    let mut value = if low > high {
        low
    } else if (low..=high).contains(&preferred) {
        preferred
    } else if low > preferred {
        low.saturating_add((h % 100) as i128).min(high)
    } else {
        high.saturating_sub((h % 100) as i128).max(low)
    };
    if let Some(m) = num(&c.multiple_of).filter(|m| *m >= 1.0 && m.fract() == 0.0) {
        let m = m as i128;
        let up = value.div_euclid(m) * m + if value.rem_euclid(m) == 0 { 0 } else { m };
        let down = value.div_euclid(m) * m;
        value = if up <= high { up } else { down };
    }
    value.clamp(i64::MIN as i128, i64::MAX as i128) as i64
}

/// A number within the bounds: an integer when one fits, else the middle
/// of the range.
fn number(c: &Constraints, h: u64, name: &str) -> Value {
    let num = |n: &Option<serde_json::Number>| n.as_ref().and_then(serde_json::Number::as_f64);
    let candidate = integer(c, h, name, i64::MIN as i128, i64::MAX as i128) as f64;
    let ok = |x: f64| {
        num(&c.minimum).is_none_or(|m| x >= m)
            && num(&c.maximum).is_none_or(|m| x <= m)
            && num(&c.exclusive_minimum).is_none_or(|m| x > m)
            && num(&c.exclusive_maximum).is_none_or(|m| x < m)
    };
    if ok(candidate) && num(&c.multiple_of).is_none_or(|m| m >= 1.0 && m.fract() == 0.0) {
        return Value::from(candidate as i64);
    }
    let low = num(&c.minimum).or(num(&c.exclusive_minimum));
    let high = num(&c.maximum).or(num(&c.exclusive_maximum));
    let x = match (low, high) {
        (Some(l), Some(hi)) => (l + hi) / 2.0,
        (Some(l), None) => l + 1.0,
        (None, Some(hi)) => hi - 1.0,
        (None, None) => candidate,
    };
    let x = match num(&c.multiple_of).filter(|m| *m > 0.0) {
        Some(m) => (x / m).round() * m,
        None => x,
    };
    serde_json::Number::from_f64(x)
        .map(Value::Number)
        .unwrap_or(Value::from(0))
}
