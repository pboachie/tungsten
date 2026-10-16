// SPDX-License-Identifier: Apache-2.0
//! A deserializer adapter that reads JSON numbers as JavaScript does: an
//! integral float below 2^53 in magnitude (`5.0`, `1e3`) is an integer, so
//! integer fields accept it. It is [`crate::util::integral_numbers`] applied
//! while decoding, so a body decoded straight from its bytes into a typed
//! value is the value its JSON form decodes to.

use std::fmt;

use serde::de::{
    DeserializeSeed, Deserializer, EnumAccess, Error, MapAccess, SeqAccess, VariantAccess, Visitor,
};

/// The integer an integral float stands for, when it is one.
fn integral(value: f64) -> Option<i64> {
    (value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0).then_some(value as i64)
}

/// A deserializer whose floats visit as integers when they are integral.
pub(crate) struct Integral<D>(pub(crate) D);

macro_rules! forward {
    ($($method:ident $(($($arg:ident : $ty:ty),*))?),* $(,)?) => {
        $(
            fn $method<V: Visitor<'de>>(self $(, $($arg: $ty),*)?, visitor: V) -> Result<V::Value, D::Error> {
                self.0.$method($($($arg,)*)? Wrap(visitor))
            }
        )*
    };
}

impl<'de, D: Deserializer<'de>> Deserializer<'de> for Integral<D> {
    type Error = D::Error;

    forward!(
        deserialize_any,
        deserialize_bool,
        deserialize_i8,
        deserialize_i16,
        deserialize_i32,
        deserialize_i64,
        deserialize_i128,
        deserialize_u8,
        deserialize_u16,
        deserialize_u32,
        deserialize_u64,
        deserialize_u128,
        deserialize_f32,
        deserialize_f64,
        deserialize_char,
        deserialize_str,
        deserialize_string,
        deserialize_bytes,
        deserialize_byte_buf,
        deserialize_option,
        deserialize_unit,
        deserialize_unit_struct(name: &'static str),
        deserialize_newtype_struct(name: &'static str),
        deserialize_seq,
        deserialize_tuple(len: usize),
        deserialize_tuple_struct(name: &'static str, len: usize),
        deserialize_map,
        deserialize_struct(name: &'static str, fields: &'static [&'static str]),
        deserialize_enum(name: &'static str, variants: &'static [&'static str]),
        deserialize_identifier,
        deserialize_ignored_any,
    );

    fn is_human_readable(&self) -> bool {
        self.0.is_human_readable()
    }
}

struct Wrap<V>(V);

macro_rules! forward_visit {
    ($($method:ident($ty:ty)),* $(,)?) => {
        $(
            fn $method<E: Error>(self, value: $ty) -> Result<V::Value, E> {
                self.0.$method(value)
            }
        )*
    };
}

impl<'de, V: Visitor<'de>> Visitor<'de> for Wrap<V> {
    type Value = V::Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.expecting(formatter)
    }

    forward_visit!(
        visit_bool(bool),
        visit_i8(i8),
        visit_i16(i16),
        visit_i32(i32),
        visit_i64(i64),
        visit_i128(i128),
        visit_u8(u8),
        visit_u16(u16),
        visit_u32(u32),
        visit_u64(u64),
        visit_u128(u128),
        visit_f32(f32),
        visit_char(char),
        visit_str(&str),
        visit_borrowed_str(&'de str),
        visit_string(String),
        visit_bytes(&[u8]),
        visit_borrowed_bytes(&'de [u8]),
        visit_byte_buf(Vec<u8>),
    );

    /// As `serde_json::Value` visits the integer `integral_numbers` makes of
    /// an integral float: non-negative as `u64`, negative as `i64`.
    fn visit_f64<E: Error>(self, value: f64) -> Result<V::Value, E> {
        match integral(value) {
            Some(int) => match u64::try_from(int) {
                Ok(unsigned) => self.0.visit_u64(unsigned),
                Err(_) => self.0.visit_i64(int),
            },
            None => self.0.visit_f64(value),
        }
    }

    fn visit_none<E: Error>(self) -> Result<V::Value, E> {
        self.0.visit_none()
    }

    fn visit_some<D: Deserializer<'de>>(self, deserializer: D) -> Result<V::Value, D::Error> {
        self.0.visit_some(Integral(deserializer))
    }

    fn visit_unit<E: Error>(self) -> Result<V::Value, E> {
        self.0.visit_unit()
    }

    fn visit_newtype_struct<D: Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<V::Value, D::Error> {
        self.0.visit_newtype_struct(Integral(deserializer))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, seq: A) -> Result<V::Value, A::Error> {
        self.0.visit_seq(WrapSeq(seq))
    }

    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<V::Value, A::Error> {
        self.0.visit_map(WrapMap(map))
    }

    fn visit_enum<A: EnumAccess<'de>>(self, data: A) -> Result<V::Value, A::Error> {
        self.0.visit_enum(WrapEnum(data))
    }
}

struct WrapSeed<S>(S);

impl<'de, S: DeserializeSeed<'de>> DeserializeSeed<'de> for WrapSeed<S> {
    type Value = S::Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<S::Value, D::Error> {
        self.0.deserialize(Integral(deserializer))
    }
}

struct WrapSeq<A>(A);

impl<'de, A: SeqAccess<'de>> SeqAccess<'de> for WrapSeq<A> {
    type Error = A::Error;

    fn next_element_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, A::Error> {
        self.0.next_element_seed(WrapSeed(seed))
    }

    fn size_hint(&self) -> Option<usize> {
        self.0.size_hint()
    }
}

struct WrapMap<A>(A);

impl<'de, A: MapAccess<'de>> MapAccess<'de> for WrapMap<A> {
    type Error = A::Error;

    fn next_key_seed<S: DeserializeSeed<'de>>(
        &mut self,
        seed: S,
    ) -> Result<Option<S::Value>, A::Error> {
        self.0.next_key_seed(WrapSeed(seed))
    }

    fn next_value_seed<S: DeserializeSeed<'de>>(&mut self, seed: S) -> Result<S::Value, A::Error> {
        self.0.next_value_seed(WrapSeed(seed))
    }

    fn size_hint(&self) -> Option<usize> {
        self.0.size_hint()
    }
}

struct WrapEnum<A>(A);

impl<'de, A: EnumAccess<'de>> EnumAccess<'de> for WrapEnum<A> {
    type Error = A::Error;
    type Variant = WrapVariant<A::Variant>;

    fn variant_seed<S: DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self::Variant), A::Error> {
        let (value, variant) = self.0.variant_seed(WrapSeed(seed))?;
        Ok((value, WrapVariant(variant)))
    }
}

struct WrapVariant<A>(A);

impl<'de, A: VariantAccess<'de>> VariantAccess<'de> for WrapVariant<A> {
    type Error = A::Error;

    fn unit_variant(self) -> Result<(), A::Error> {
        self.0.unit_variant()
    }

    fn newtype_variant_seed<S: DeserializeSeed<'de>>(self, seed: S) -> Result<S::Value, A::Error> {
        self.0.newtype_variant_seed(WrapSeed(seed))
    }

    fn tuple_variant<V: Visitor<'de>>(self, len: usize, visitor: V) -> Result<V::Value, A::Error> {
        self.0.tuple_variant(len, Wrap(visitor))
    }

    fn struct_variant<V: Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, A::Error> {
        self.0.struct_variant(fields, Wrap(visitor))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::Integral;
    use crate::util::integral_numbers;

    fn direct(text: &str) -> Value {
        let mut de = serde_json::Deserializer::from_str(text);
        let value = serde::Deserialize::deserialize(Integral(&mut de)).expect("parses");
        de.end().expect("ends");
        value
    }

    fn through_value(text: &str) -> Value {
        let mut value: Value = serde_json::from_str(text).expect("parses");
        integral_numbers(&mut value);
        value
    }

    #[test]
    fn floats_visit_as_the_integers_integral_numbers_makes_of_them() {
        for text in [
            r#"{"a": 2.0, "b": 1.5, "c": [3.0, {"d": -4.0e0}], "e": 1e300, "f": -0.0}"#,
            "9007199254740992.0",
            "9007199254740991.0",
            "-9007199254740991.0",
            "1e3",
            "[18446744073709551615, -9223372036854775808, 0.5]",
        ] {
            let a = direct(text);
            let b = through_value(text);
            assert_eq!(a, b, "{text}");
            assert_eq!(a.to_string(), b.to_string(), "{text}");
        }
    }

    #[test]
    fn typed_fields_accept_integral_floats() {
        #[derive(Debug, PartialEq, serde::Deserialize)]
        struct Row {
            count: i64,
            size: u32,
            ratio: f64,
            #[serde(flatten)]
            extra: std::collections::BTreeMap<String, Value>,
        }
        let mut de = serde_json::Deserializer::from_str(
            r#"{"count": 5.0, "size": 2e1, "ratio": 7.0, "other": 1.0, "fine": 1.25}"#,
        );
        let row: Row = serde::Deserialize::deserialize(Integral(&mut de)).expect("decodes");
        assert_eq!(row.count, 5);
        assert_eq!(row.size, 20);
        assert_eq!(row.ratio, 7.0);
        assert_eq!(row.extra["other"], Value::from(1));
        assert!(row.extra["other"].is_u64());
        assert_eq!(row.extra["fine"], Value::from(1.25));
        let mut de = serde_json::Deserializer::from_str("5.5");
        assert!(<i64 as serde::Deserialize>::deserialize(Integral(&mut de)).is_err());
    }
}
