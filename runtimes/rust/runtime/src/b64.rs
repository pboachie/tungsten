// SPDX-License-Identifier: Apache-2.0
//! `#[serde(with = "tungsten_runtime::b64")]` for `Vec<u8>` fields that are
//! base64 text on the wire (`format: byte`).

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Deserializer, Serializer};

pub fn serialize<S: Serializer>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&STANDARD.encode(bytes))
}

pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<u8>, D::Error> {
    let text = String::deserialize(deserializer)?;
    STANDARD.decode(text).map_err(serde::de::Error::custom)
}
