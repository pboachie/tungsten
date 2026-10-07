// SPDX-License-Identifier: AGPL-3.0-only
//! YAML → JSON conversion. PHASE-1 STUB: no spans.

use yaml_rust2::{Yaml, YamlLoader};

pub(crate) fn to_json(text: &str) -> Result<serde_json::Value, String> {
    let docs = YamlLoader::load_from_str(text).map_err(|e| e.to_string())?;
    let doc = docs.into_iter().next().unwrap_or(Yaml::Null);
    convert(&doc)
}

fn convert(y: &Yaml) -> Result<serde_json::Value, String> {
    use serde_json::Value as J;
    Ok(match y {
        Yaml::Null | Yaml::BadValue => J::Null,
        Yaml::Boolean(b) => J::Bool(*b),
        Yaml::Integer(i) => J::from(*i),
        Yaml::Real(s) => s
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(J::Number)
            .unwrap_or(J::String(s.clone())),
        Yaml::String(s) => J::String(s.clone()),
        Yaml::Array(a) => J::Array(a.iter().map(convert).collect::<Result<_, _>>()?),
        Yaml::Hash(h) => {
            let mut m = serde_json::Map::new();
            for (k, v) in h {
                let key = match k {
                    Yaml::String(s) => s.clone(),
                    Yaml::Integer(i) => i.to_string(),
                    Yaml::Boolean(b) => b.to_string(),
                    other => return Err(format!("unsupported mapping key {other:?}")),
                };
                m.insert(key, convert(v)?);
            }
            J::Object(m)
        }
        Yaml::Alias(_) => return Err("YAML aliases are not supported".into()),
    })
}
