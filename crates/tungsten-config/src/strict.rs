// SPDX-License-Identifier: AGPL-3.0-only
//! A strict deserializer over `serde_json::Value`.
//!
//! `serde_json::Value` lets a struct be read from a sequence (positional
//! fields), so `api: [demo]` would silently become `api: { name: demo }`.
//! The manifest requires mappings for structs and maps and sequences for
//! sequences; everything else is delegated to `serde_json`.

use serde::de::value::BorrowedStrDeserializer;
use serde::de::{
    DeserializeSeed, Deserializer, Error as _, MapAccess, SeqAccess, Unexpected, Visitor,
};
use serde_json::{Error, Map, Value};

#[derive(Debug, Clone, Copy)]
pub(crate) struct Strict<'a>(pub &'a Value);

macro_rules! delegate {
    ($($method:ident)*) => {
        $(
            fn $method<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
                self.0.$method(visitor)
            }
        )*
    };
}

impl<'de> Deserializer<'de> for Strict<'de> {
    type Error = Error;

    fn deserialize_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Value::Array(items) => visitor.visit_seq(Seq(items.iter())),
            Value::Object(map) => visitor.visit_map(Entries::new(map)),
            scalar => scalar.deserialize_any(visitor),
        }
    }

    delegate! {
        deserialize_bool deserialize_i8 deserialize_i16 deserialize_i32 deserialize_i64
        deserialize_i128 deserialize_u8 deserialize_u16 deserialize_u32 deserialize_u64
        deserialize_u128 deserialize_f32 deserialize_f64 deserialize_char deserialize_str
        deserialize_string deserialize_bytes deserialize_byte_buf deserialize_unit
        deserialize_identifier
    }

    fn deserialize_option<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Value::Null => visitor.visit_none(),
            _ => visitor.visit_some(self),
        }
    }

    fn deserialize_unit_struct<V: Visitor<'de>>(
        self,
        name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.0.deserialize_unit_struct(name, visitor)
    }

    fn deserialize_newtype_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        visitor: V,
    ) -> Result<V::Value, Error> {
        visitor.visit_newtype_struct(self)
    }

    fn deserialize_seq<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Value::Array(items) => visitor.visit_seq(Seq(items.iter())),
            other => Err(Error::invalid_type(unexpected(other), &visitor)),
        }
    }

    fn deserialize_tuple<V: Visitor<'de>>(
        self,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_tuple_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _len: usize,
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_seq(visitor)
    }

    fn deserialize_map<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        match self.0 {
            Value::Object(map) => visitor.visit_map(Entries::new(map)),
            other => Err(Error::invalid_type(unexpected(other), &visitor)),
        }
    }

    fn deserialize_struct<V: Visitor<'de>>(
        self,
        _name: &'static str,
        _fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.deserialize_map(visitor)
    }

    /// Enums keep `serde_json`'s representation: a unit variant is read
    /// from a string. Manifests with richer alternatives (composite parts,
    /// agent.yml shorthands) read them with their own visitors.
    fn deserialize_enum<V: Visitor<'de>>(
        self,
        name: &'static str,
        variants: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Error> {
        self.0.deserialize_enum(name, variants, visitor)
    }

    fn deserialize_ignored_any<V: Visitor<'de>>(self, visitor: V) -> Result<V::Value, Error> {
        visitor.visit_unit()
    }
}

struct Seq<'a>(std::slice::Iter<'a, Value>);

impl<'de> SeqAccess<'de> for Seq<'de> {
    type Error = Error;

    fn next_element_seed<T: DeserializeSeed<'de>>(
        &mut self,
        seed: T,
    ) -> Result<Option<T::Value>, Error> {
        self.0
            .next()
            .map(|v| seed.deserialize(Strict(v)))
            .transpose()
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.0.len())
    }
}

struct Entries<'a> {
    iter: serde_json::map::Iter<'a>,
    value: Option<&'a Value>,
}

impl<'a> Entries<'a> {
    fn new(map: &'a Map<String, Value>) -> Self {
        Self {
            iter: map.iter(),
            value: None,
        }
    }
}

impl<'de> MapAccess<'de> for Entries<'de> {
    type Error = Error;

    fn next_key_seed<K: DeserializeSeed<'de>>(
        &mut self,
        seed: K,
    ) -> Result<Option<K::Value>, Error> {
        let Some((key, value)) = self.iter.next() else {
            return Ok(None);
        };
        self.value = Some(value);
        seed.deserialize(BorrowedStrDeserializer::new(key))
            .map(Some)
    }

    fn next_value_seed<V: DeserializeSeed<'de>>(&mut self, seed: V) -> Result<V::Value, Error> {
        match self.value.take() {
            Some(value) => seed.deserialize(Strict(value)),
            None => Err(Error::custom("map value requested before its key")),
        }
    }

    fn size_hint(&self) -> Option<usize> {
        Some(self.iter.len())
    }
}

fn unexpected(value: &Value) -> Unexpected<'_> {
    match value {
        Value::Null => Unexpected::Unit,
        Value::Bool(b) => Unexpected::Bool(*b),
        Value::Number(n) => match (n.as_u64(), n.as_i64(), n.as_f64()) {
            (Some(u), _, _) => Unexpected::Unsigned(u),
            (_, Some(i), _) => Unexpected::Signed(i),
            (_, _, Some(f)) => Unexpected::Float(f),
            _ => Unexpected::Other("number"),
        },
        Value::String(s) => Unexpected::Str(s),
        Value::Array(_) => Unexpected::Seq,
        Value::Object(_) => Unexpected::Map,
    }
}
