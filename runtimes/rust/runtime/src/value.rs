// SPDX-License-Identifier: Apache-2.0
//! Value types that generated models and requests use.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};

/// A field that may be absent, `null`, or a value (presence
/// `OptionalNullable`). Use it with
/// `#[serde(default, skip_serializing_if = "Patch::is_undefined")]`.
#[derive(Debug, Clone, Default, PartialEq)]
pub enum Patch<T> {
    /// The member is absent; not serialized.
    #[default]
    Undefined,
    /// The member is present and `null`.
    Null,
    Value(T),
}

impl<T> Patch<T> {
    pub fn is_undefined(&self) -> bool {
        matches!(self, Patch::Undefined)
    }

    pub fn as_value(&self) -> Option<&T> {
        match self {
            Patch::Value(v) => Some(v),
            _ => None,
        }
    }
}

impl<T: Serialize> Serialize for Patch<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Patch::Undefined | Patch::Null => serializer.serialize_none(),
            Patch::Value(v) => v.serialize(serializer),
        }
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for Patch<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Option::<T>::deserialize(deserializer)? {
            Some(v) => Patch::Value(v),
            None => Patch::Null,
        })
    }
}

/// Key of the object that carries bytes inside an arguments object (which is
/// JSON): `{"$tungsten_binary": "<base64>", "filename": ..., "content_type": ...}`.
pub const BINARY_KEY: &str = "$tungsten_binary";

/// Bytes for a `bytes` or `multipart` request body, with the optional
/// filename and content type of a multipart part. In the arguments object it
/// is the tagged object above, which only the request encoder interprets;
/// base64 text in JSON models (`format: byte`) uses [`crate::b64`] instead.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Binary {
    pub data: Vec<u8>,
    pub filename: Option<String>,
    pub content_type: Option<String>,
}

impl Binary {
    pub fn new(data: impl Into<Vec<u8>>) -> Self {
        Binary {
            data: data.into(),
            filename: None,
            content_type: None,
        }
    }

    pub fn with_filename(mut self, filename: impl Into<String>) -> Self {
        self.filename = Some(filename.into());
        self
    }

    pub fn with_content_type(mut self, content_type: impl Into<String>) -> Self {
        self.content_type = Some(content_type.into());
        self
    }
}

impl fmt::Debug for Binary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Binary({} bytes)", self.data.len())
    }
}

impl From<Vec<u8>> for Binary {
    fn from(data: Vec<u8>) -> Self {
        Binary::new(data)
    }
}

impl Serialize for Binary {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let len =
            1 + usize::from(self.filename.is_some()) + usize::from(self.content_type.is_some());
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry(BINARY_KEY, &STANDARD.encode(&self.data))?;
        if let Some(filename) = &self.filename {
            map.serialize_entry("filename", filename)?;
        }
        if let Some(content_type) = &self.content_type {
            map.serialize_entry("content_type", content_type)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Binary {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Binary;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a binary value ({\"$tungsten_binary\": \"<base64>\"})")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Binary, A::Error> {
                let mut out = Binary::default();
                let mut seen = false;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        BINARY_KEY => {
                            let text: String = map.next_value()?;
                            out.data = STANDARD.decode(text).map_err(de::Error::custom)?;
                            seen = true;
                        }
                        "filename" => out.filename = map.next_value()?,
                        "content_type" => out.content_type = map.next_value()?,
                        other => {
                            return Err(de::Error::unknown_field(
                                other,
                                &[BINARY_KEY, "filename", "content_type"],
                            ));
                        }
                    }
                }
                if seen {
                    Ok(out)
                } else {
                    Err(de::Error::missing_field(BINARY_KEY))
                }
            }
        }
        deserializer.deserialize_map(V)
    }
}
