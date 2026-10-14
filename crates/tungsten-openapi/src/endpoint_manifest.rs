// SPDX-License-Identifier: AGPL-3.0-only
//! Versioned HTTP routing for declared JSON Schema models. All model reads are
//! local, bounded and confined to caller-approved canonical roots.

use crate::LoadOptions;
use schemars::JsonSchema;
use serde::{Deserialize, de::IntoDeserializer};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use tungsten_core::{Diagnostic, Digest, SourceMap};

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Manifest {
    endpoint_manifest: u32,
    title: String,
    version: String,
    base_url: String,
    #[serde(default)]
    models: BTreeMap<String, Value>,
    #[serde(default)]
    auth: BTreeMap<String, Auth>,
    endpoints: Vec<Endpoint>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Endpoint {
    id: String,
    method: String,
    path: String,
    #[serde(default)]
    parameters: Vec<Parameter>,
    #[serde(default)]
    request: BTreeMap<String, Value>,
    responses: BTreeMap<String, Response>,
    #[serde(default)]
    security: Vec<BTreeMap<String, Vec<String>>>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Parameter {
    name: String,
    location: String,
    #[serde(default)]
    required: bool,
    schema: Value,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Response {
    description: String,
    #[serde(default)]
    content: BTreeMap<String, Value>,
}
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Auth {
    Bearer,
    Basic,
    ApiKey { name: String, location: String },
}

fn error(p: &str, message: &str) -> Diagnostic {
    Diagnostic::error("TG0112", message).at("endpoint-manifest", p, None)
}
fn locate(mut diagnostic: Diagnostic, file: &Path) -> Diagnostic {
    for label in &mut diagnostic.labels {
        if label.file == "endpoint-manifest" {
            label.file = file.to_string_lossy().into_owned();
        }
    }
    diagnostic
}
fn esc(s: &str) -> String {
    s.replace('~', "~0").replace('/', "~1")
}
fn identifier(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 128
        && s.bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
}

/// Machine-readable shape of the versioned routing manifest. Model keyword,
/// routing and reference constraints are additionally validated by `load`.
pub fn manifest_schema() -> Value {
    serde_json::to_value(schemars::schema_for!(Manifest)).expect("JSON Schema is serializable")
}

/// Load a version-1 manifest and bundle its JSON Schema 2020-12 structural
/// subset into portable OpenAPI 3.1. No auth values, examples or defaults are
/// accepted. `allowed_roots` explicitly bounds every referenced local file.
pub fn load(
    path: &Path,
    allowed_roots: &[PathBuf],
    opts: &LoadOptions,
) -> Result<Value, Diagnostic> {
    load_inner(path, allowed_roots, opts).map_err(|diagnostic| locate(diagnostic, path))
}

fn load_inner(
    path: &Path,
    allowed_roots: &[PathBuf],
    opts: &LoadOptions,
) -> Result<Value, Diagnostic> {
    let roots: Vec<PathBuf> = allowed_roots
        .iter()
        .map(|r| {
            std::fs::canonicalize(r).map_err(|_| error("", "cannot resolve an allowed model root"))
        })
        .collect::<Result<_, _>>()?;
    let path =
        std::fs::canonicalize(path).map_err(|_| error("", "cannot resolve endpoint manifest"))?;
    let mut reader = Reader {
        roots,
        opts,
        bytes: 0,
        documents: BTreeMap::new(),
        names: BTreeMap::new(),
        pending: vec![],
        nodes: 0,
    };
    let value = reader.read(&path)?;
    let manifest: Manifest = serde_path_to_error::deserialize(value.clone().into_deserializer())
        .map_err(|failure| {
            let pointer = failure.path().iter().filter_map(|segment| match segment {
                serde_path_to_error::Segment::Seq { index } => Some(index.to_string()),
                serde_path_to_error::Segment::Map { key } => Some(esc(key)),
                serde_path_to_error::Segment::Enum { variant } => Some(esc(variant)),
                serde_path_to_error::Segment::Unknown => None,
            }).map(|segment| format!("/{segment}")).collect::<String>();
            error(&pointer, "invalid endpoint manifest field; check the versioned manifest schema and required HTTP routing metadata")
        })?;
    if manifest.endpoint_manifest != 1 {
        return Err(error(
            "/endpoint_manifest",
            "unsupported endpoint manifest version",
        ));
    }
    if manifest.endpoints.is_empty() || manifest.endpoints.len() > 10_000 {
        return Err(error(
            "/endpoints",
            "provide 1 to 10000 endpoints; a model alone cannot declare HTTP routing",
        ));
    }
    let base = url::Url::parse(&manifest.base_url).map_err(|_| {
        error(
            "/base_url",
            "base_url must be an absolute HTTP origin/path without credentials",
        )
    })?;
    if !matches!(base.scheme(), "http" | "https")
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(error(
            "/base_url",
            "base_url must be HTTP(S) without credentials, query or fragment",
        ));
    }
    reader.documents.insert(path.clone(), value);
    let mut schemas = BTreeMap::new();
    for (name, model) in manifest.models {
        if !identifier(&name) || name.starts_with("external_") {
            return Err(error(
                "/models",
                "model names must be simple identifiers outside the reserved external_ namespace",
            ));
        }
        schemas.insert(
            name.clone(),
            reader.schema(model, &path, &format!("/models/{}", esc(&name)), 0)?,
        );
    }
    let mut security = BTreeMap::new();
    for (name, auth) in manifest.auth {
        if !identifier(&name) {
            return Err(error(
                "/auth",
                "auth scheme names must be simple identifiers",
            ));
        }
        let scheme = match auth {
            Auth::Bearer => json!({"type":"http","scheme":"bearer"}),
            Auth::Basic => json!({"type":"http","scheme":"basic"}),
            Auth::ApiKey { name, location } => {
                if !identifier(&name) || !matches!(location.as_str(), "header" | "query" | "cookie")
                {
                    return Err(error(
                        "/auth",
                        "API key requires a simple name and header, query or cookie location",
                    ));
                }
                json!({"type":"apiKey","name":name,"in":location})
            }
        };
        security.insert(name, scheme);
    }
    let mut paths = serde_json::Map::new();
    let mut ids = BTreeSet::new();
    for (index, endpoint) in manifest.endpoints.into_iter().enumerate() {
        let pointer = format!("/endpoints/{index}");
        if !identifier(&endpoint.id) || !ids.insert(endpoint.id.clone()) {
            return Err(error(
                &pointer,
                "endpoint IDs must be unique simple identifiers",
            ));
        }
        let method = endpoint.method.to_ascii_lowercase();
        if ![
            "get", "put", "post", "delete", "options", "head", "patch", "trace",
        ]
        .contains(&method.as_str())
        {
            return Err(error(&pointer, "unsupported HTTP method"));
        }
        if !endpoint.path.starts_with('/')
            || endpoint.path.starts_with("//")
            || endpoint.path.contains(['?', '#', '\\'])
            || endpoint.path.chars().any(char::is_control)
        {
            return Err(error(
                &pointer,
                "endpoint path must be an explicit HTTP path template without query or fragment",
            ));
        }
        let mut parameter_names = BTreeSet::new();
        let mut path_names = BTreeSet::new();
        let mut parameters = vec![];
        for (i, param) in endpoint.parameters.into_iter().enumerate() {
            let pp = format!("{pointer}/parameters/{i}");
            if !identifier(&param.name)
                || !matches!(param.location.as_str(), "path" | "query" | "header")
                || !parameter_names.insert((param.location.clone(), param.name.clone()))
            {
                return Err(error(
                    &pp,
                    "parameter requires a unique name and path, query or header location",
                ));
            }
            if param.location == "path" {
                if !param.required {
                    return Err(error(&pp, "path parameter must be required"));
                }
                path_names.insert(param.name.clone());
            }
            if matches!(
                param.name.to_ascii_lowercase().as_str(),
                "authorization" | "cookie" | "proxy-authorization"
            ) {
                return Err(error(
                    &pp,
                    "credential parameters belong in auth scheme references",
                ));
            }
            parameters.push(json!({"name":param.name,"in":param.location,"required":param.required,"schema":reader.schema(param.schema,&path,&format!("{pp}/schema"),0)?}));
        }
        let mut template_names = BTreeSet::new();
        for part in endpoint.path.split('/') {
            if part.contains(['{', '}']) {
                let Some(name) = part.strip_prefix('{').and_then(|p| p.strip_suffix('}')) else {
                    return Err(error(
                        &pointer,
                        "path templates must occupy a complete path segment",
                    ));
                };
                if !identifier(name) {
                    return Err(error(&pointer, "invalid path template parameter"));
                }
                template_names.insert(name.to_string());
            }
        }
        if path_names != template_names {
            return Err(error(
                &pointer,
                "path template and required path parameters do not match",
            ));
        }
        for requirement in &endpoint.security {
            for (name, scopes) in requirement {
                if !security.contains_key(name) || !scopes.is_empty() {
                    return Err(error(
                        &pointer,
                        "security must reference a declared scheme with no OAuth scopes in this subset",
                    ));
                }
            }
        }
        let mut op = json!({"operationId":endpoint.id,"parameters":parameters,"security":endpoint.security,"x-tungsten-imported":true});
        if !endpoint.request.is_empty() {
            op["requestBody"] = json!({"content":reader.content(endpoint.request,&path,&format!("{pointer}/request"))?});
        }
        if endpoint.responses.is_empty() {
            return Err(error(
                &pointer,
                "endpoint requires at least one response status",
            ));
        }
        let mut responses = BTreeMap::new();
        for (status, response) in endpoint.responses {
            if status != "default"
                && !status
                    .parse::<u16>()
                    .is_ok_and(|s| (100..=599).contains(&s) && status.len() == 3)
            {
                return Err(error(
                    &pointer,
                    "response status must be 100 through 599 or default",
                ));
            }
            responses.insert(status.clone(),json!({"description":response.description,"content":reader.content(response.content,&path,&format!("{pointer}/responses/{status}/content"))?}));
        }
        op["responses"] = json!(responses);
        let item = paths.entry(endpoint.path).or_insert_with(|| json!({}));
        if item.get(&method).is_some() {
            return Err(error(&pointer, "duplicate method/path endpoint"));
        }
        item[method] = op;
    }
    while let Some(file) = reader.pending.pop() {
        let model = reader
            .documents
            .get(&file)
            .cloned()
            .ok_or_else(|| error("", "missing loaded model"))?;
        let name = reader.names[&file].clone();
        schemas.insert(name, reader.schema(model, &file, "", 0)?);
    }
    Ok(
        json!({"openapi":"3.1.0","info":{"title":manifest.title,"version":manifest.version},"servers":[{"url":base.as_str()}],"components":{"schemas":schemas,"securitySchemes":security},"paths":paths}),
    )
}

struct Reader<'a> {
    roots: Vec<PathBuf>,
    opts: &'a LoadOptions,
    bytes: usize,
    documents: BTreeMap<PathBuf, Value>,
    names: BTreeMap<PathBuf, String>,
    pending: Vec<PathBuf>,
    nodes: usize,
}
impl Reader<'_> {
    fn read(&mut self, path: &Path) -> Result<Value, Diagnostic> {
        self.read_inner(path)
            .map_err(|diagnostic| locate(diagnostic, path))
    }
    fn read_inner(&mut self, path: &Path) -> Result<Value, Diagnostic> {
        if !self.roots.iter().any(|root| path.starts_with(root)) {
            return Err(error("", "model reference escapes allowed roots"));
        }
        let metadata = std::fs::metadata(path).map_err(|_| error("", "cannot read local model"))?;
        if !metadata.is_file() {
            return Err(error("", "model must be a regular file"));
        }
        let mut text = String::new();
        std::fs::File::open(path)
            .map_err(|_| error("", "cannot open model"))?
            .take(self.opts.max_bytes as u64 + 1)
            .read_to_string(&mut text)
            .map_err(|_| error("", "model must be UTF-8 text"))?;
        self.bytes = self.bytes.saturating_add(text.len());
        if text.len() > self.opts.max_bytes || self.bytes > self.opts.max_bytes.saturating_mul(2) {
            return Err(error("", "model byte budget exceeded"));
        }
        let mut sources = SourceMap::new();
        let source = sources.add("model", text.clone());
        crate::parse::parse(
            &path.to_string_lossy(),
            &text,
            source,
            self.opts.max_depth.min(128),
        )
        .map(|p| p.value)
        .map_err(|_| error("", "invalid JSON/YAML model or nesting limit exceeded"))
    }
    fn content(
        &mut self,
        content: BTreeMap<String, Value>,
        path: &Path,
        p: &str,
    ) -> Result<Value, Diagnostic> {
        let mut out = BTreeMap::new();
        for (media, schema) in content {
            if !media.contains('/') || media.chars().any(char::is_control) {
                return Err(error(p, "content type must be a media type"));
            }
            out.insert(
                media.clone(),
                json!({"schema":self.schema(schema,path,&format!("{p}/{}",esc(&media)),0)?}),
            );
        }
        Ok(json!(out))
    }
    fn schema(
        &mut self,
        v: Value,
        file: &Path,
        p: &str,
        depth: usize,
    ) -> Result<Value, Diagnostic> {
        self.schema_inner(v, file, p, depth)
            .map_err(|diagnostic| locate(diagnostic, file))
    }
    fn schema_inner(
        &mut self,
        mut v: Value,
        file: &Path,
        p: &str,
        depth: usize,
    ) -> Result<Value, Diagnostic> {
        self.nodes += 1;
        if depth > self.opts.max_depth.min(128) || self.nodes > 500_000 {
            return Err(error(p, "schema node/depth budget exceeded"));
        }
        if v.is_boolean() {
            return Ok(v);
        }
        let Some(m) = v.as_object_mut() else {
            return Err(error(p, "schema must be an object or boolean"));
        };
        let allowed = [
            "$schema",
            "$ref",
            "$defs",
            "type",
            "properties",
            "required",
            "additionalProperties",
            "items",
            "prefixItems",
            "allOf",
            "anyOf",
            "oneOf",
            "not",
            "minimum",
            "maximum",
            "exclusiveMinimum",
            "exclusiveMaximum",
            "multipleOf",
            "minLength",
            "maxLength",
            "pattern",
            "minItems",
            "maxItems",
            "uniqueItems",
            "minProperties",
            "maxProperties",
            "readOnly",
            "writeOnly",
        ];
        if m.keys().any(|k| !allowed.contains(&k.as_str())) {
            return Err(error(
                p,
                "unsupported schema keyword; annotations, literals, IDs and dynamic references are not accepted",
            ));
        }
        if let Some(dialect) = m.remove("$schema")
            && dialect != json!("https://json-schema.org/draft/2020-12/schema")
        {
            return Err(error(p, "only JSON Schema 2020-12 is supported"));
        }
        if let Some(reference) = m.get_mut("$ref") {
            let Some(reference_text) = reference.as_str() else {
                return Err(error(p, "$ref must be a string"));
            };
            let (relative, fragment) = reference_text
                .split_once('#')
                .unwrap_or((reference_text, ""));
            if relative.contains([':', '\\', '?', '%'])
                || relative.starts_with('/')
                || fragment.contains('%')
            {
                return Err(error(
                    p,
                    "only local relative file references and JSON pointers are supported",
                ));
            }
            if !fragment.is_empty() && !fragment.starts_with('/') {
                return Err(error(p, "reference fragment must be a JSON pointer"));
            }
            let target = if relative.is_empty() {
                file.to_path_buf()
            } else {
                std::fs::canonicalize(file.parent().unwrap_or(Path::new(".")).join(relative))
                    .map_err(|_| error(p, "local schema reference cannot be resolved"))?
            };
            if !self.documents.contains_key(&target) {
                if self.documents.len() >= 64 {
                    return Err(error(p, "referenced model file limit exceeded"));
                }
                let doc = self.read(&target)?;
                let (root_index, relative_path) = self
                    .roots
                    .iter()
                    .enumerate()
                    .find_map(|(index, root)| {
                        target.strip_prefix(root).ok().map(|path| (index, path))
                    })
                    .ok_or_else(|| error(p, "model reference escapes allowed roots"))?;
                let stable_path = format!(
                    "{root_index}:{}",
                    relative_path.to_string_lossy().replace('\\', "/")
                );
                let digest = Digest::of(stable_path.as_bytes()).0.replace(':', "_");
                let name = format!("external_{digest}");
                if self.names.values().any(|n| n == &name) {
                    return Err(error(
                        p,
                        "model names collide; use distinct relative reference paths",
                    ));
                }
                self.names.insert(target.clone(), name);
                self.documents.insert(target.clone(), doc);
                self.pending.push(target.clone());
            }
            if self.documents[&target].pointer(fragment).is_none() {
                return Err(error(p, "local schema reference pointer does not exist"));
            }
            *reference = if let Some(name) = self.names.get(&target) {
                json!(format!("#/components/schemas/{name}{fragment}"))
            } else {
                let Some(model) = fragment.strip_prefix("/models/") else {
                    return Err(error(p, "manifest references must point into /models"));
                };
                json!(format!("#/components/schemas/{model}"))
            };
        }
        for key in ["properties", "$defs"] {
            if let Some(map) = m.get_mut(key) {
                let Some(map) = map.as_object_mut() else {
                    return Err(error(p, "schema map must be an object"));
                };
                for (name, schema) in map {
                    *schema = self.schema(
                        schema.take(),
                        file,
                        &format!("{p}/{key}/{}", esc(name)),
                        depth + 1,
                    )?;
                }
            }
        }
        for key in ["items", "additionalProperties", "not"] {
            if let Some(schema) = m.get_mut(key) {
                *schema = self.schema(schema.take(), file, &format!("{p}/{key}"), depth + 1)?;
            }
        }
        for key in ["allOf", "anyOf", "oneOf", "prefixItems"] {
            if let Some(list) = m.get_mut(key) {
                let Some(list) = list.as_array_mut() else {
                    return Err(error(p, "schema combinator must be an array"));
                };
                for (i, schema) in list.iter_mut().enumerate() {
                    *schema =
                        self.schema(schema.take(), file, &format!("{p}/{key}/{i}"), depth + 1)?;
                }
            }
        }
        Ok(v)
    }
}
