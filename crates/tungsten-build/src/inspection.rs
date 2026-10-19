// SPDX-License-Identifier: AGPL-3.0-only
//! Pure configuration capture and redacted inspection, without loading API inputs.

use std::collections::BTreeMap;
use std::path::Path;

use tungsten_config::inspection::ConfigurationInspection;
use tungsten_config::profiles::{CapturedProfile, EffectiveConfiguration};
use tungsten_config::{ParsedManifest, TungstenConfig};
use tungsten_core::{Diagnostics, SourceMap};

use crate::CompileOptions;

/// Invocation-local raw capture for trusted configuration consumers. Raw source
/// text and configuration are sensitive; use [`inspect_project`] for export.
#[derive(Debug)]
pub struct ProjectConfiguration {
    pub parsed: ParsedManifest,
    pub effective: Option<EffectiveConfiguration>,
    pub profile_sources: Vec<CapturedProfile>,
}

/// Safe public inspection. Sources contain blank excerpts preserving locations.
#[derive(Debug)]
pub struct InspectedProject {
    pub configuration: Option<ConfigurationInspection>,
    pub diagnostics: Diagnostics,
    pub sources: SourceMap,
}

/// Reuses the compiler's config-only capture and provider overrides. Never reads
/// API/agent content, acquires inputs, executes tools, or publishes after cancellation.
pub fn inspect_project(manifest: &Path, options: &CompileOptions) -> InspectedProject {
    let loaded = crate::driver::load_project_configuration(manifest, options);
    let mut diagnostics = loaded.parsed.diagnostics;
    let configuration = match (loaded.parsed.config.as_ref(), loaded.effective.as_ref()) {
        (Some(config), Some(effective)) => {
            let (mut configuration, checked) =
                tungsten_config::inspection::inspect(config, effective);
            if let Some(policy) = &options.policy {
                configuration.policy = policy.inspection();
            }
            diagnostics.extend(checked);
            Some(configuration)
        }
        _ => None,
    };
    let mut sources = SourceMap::new();
    let mut names = BTreeMap::new();
    let mut add = |source: &tungsten_config::ManifestSource, name: String| {
        // Mask text by character, then translate spans into that safe text so
        // Unicode before a diagnostic retains its original line and column.
        let text: String = source
            .text()
            .chars()
            .map(|character| {
                if character == '\n' || character == '\r' {
                    character
                } else {
                    ' '
                }
            })
            .collect();
        let id = sources.add(&name, text);
        source.attach_spans(id, &mut diagnostics);
        if !source.text().is_ascii() {
            let mut offsets: BTreeMap<u32, u32> = diagnostics
                .0
                .iter()
                .flat_map(|diagnostic| &diagnostic.labels)
                .filter(|label| label.file == source.name())
                .filter_map(|label| label.span)
                .flat_map(|span| [(span.start, 0), (span.end, 0)])
                .collect();
            let mut characters = 0;
            for (byte, _) in source.text().char_indices() {
                if let Some(offset) = offsets.get_mut(&(byte as u32)) {
                    *offset = characters;
                }
                characters += 1;
            }
            if let Some(offset) = offsets.get_mut(&(source.text().len() as u32)) {
                *offset = characters;
            }
            for label in diagnostics
                .0
                .iter_mut()
                .flat_map(|diagnostic| &mut diagnostic.labels)
            {
                if label.file == source.name()
                    && let Some(span) = &mut label.span
                {
                    span.start = offsets[&span.start];
                    span.end = offsets[&span.end];
                }
            }
        }
        names.insert(source.name().to_string(), name);
    };
    add(&loaded.parsed.source, "project".into());
    for (index, source) in loaded.profile_sources.iter().enumerate() {
        add(&source.source, format!("profile[{index}]"));
    }
    let manifest_schema = TungstenConfig::json_schema();
    let profile_schema = tungsten_config::profiles::json_schema();
    for diagnostic in &mut diagnostics.0 {
        // Shared policy diagnostics contain only fixed text, opaque ordinals,
        // known pointers and digests. Do not erase their protected provenance.
        if matches!(diagnostic.code.as_str(), "TG0809" | "TG0810" | "TG0811") {
            continue;
        }
        // Parse/semantic diagnostics can include arbitrary invalid values,
        // unknown keys, filenames or credential-bearing input URLs.
        let (message, help) = match diagnostic.code.as_str() {
            "TG0101" => (
                "configuration file could not be read",
                "check the selected local manifest/profile exists and is readable",
            ),
            "TG0105" => (
                "configuration exceeds its size or nesting limit",
                "reduce the manifest/profile size or nesting to the documented limits",
            ),
            "TG0601" => (
                "configuration is not valid YAML or JSON",
                "fix syntax at the indicated source location; authored text is omitted",
            ),
            "TG0711" => (
                "invalid supported target option",
                "use the supported option schema from `tungsten capabilities typescript --json`; scalar warnings use defaults and invalid custom dependencies prevent generation",
            ),
            _ => (
                "configuration declaration is invalid",
                "check the manifest/profile schema, supported version, relative selections, include roots, cycles and documented graph limits",
            ),
        };
        diagnostic.message = message.into();
        diagnostic.help = Some(help.into());
        for label in &mut diagnostic.labels {
            label.file = names
                .get(&label.file)
                .cloned()
                .unwrap_or_else(|| "project".into());
            label.pointer = safe_pointer(
                &label.pointer,
                if label.file == "project" {
                    &manifest_schema
                } else {
                    &profile_schema
                },
            );
            label.message = None;
        }
    }
    diagnostics.sort();
    InspectedProject {
        configuration: configuration.filter(|_| !options.load.cancellation.is_cancelled()),
        diagnostics,
        sources,
    }
}

fn safe_pointer(pointer: &str, root: &serde_json::Value) -> String {
    let mut schema = root.clone();
    let mut path = String::new();
    for part in pointer.split('/').skip(1) {
        // Resolve only local declarations. Missing/unknown schemas never make
        // arbitrary map keys (including numeric identifiers) exportable.
        while let Some(reference) = schema.get("$ref").and_then(serde_json::Value::as_str) {
            let Some(resolved) = reference.strip_prefix('#').and_then(|p| root.pointer(p)) else {
                break;
            };
            schema = resolved.clone();
        }
        let decoded = part.replace("~1", "/").replace("~0", "~");
        let mut visible = false;
        if let Some(property) = schema.get("properties").and_then(|p| p.get(&decoded)) {
            schema = property.clone();
            visible = true;
        } else if path == "/targets" {
            if let Some(target) = tungsten_config::capabilities::target(&decoded) {
                schema = target
                    .options_schema
                    .map(|s| s.json_schema())
                    .unwrap_or_default();
                visible = true;
            } else {
                schema = serde_json::Value::Null;
            }
        } else if schema.get("type").and_then(serde_json::Value::as_str) == Some("array") {
            visible = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
            schema = schema.get("items").cloned().unwrap_or_default();
        } else {
            schema = schema
                .get("additionalProperties")
                .cloned()
                .unwrap_or_default();
        }
        path =
            tungsten_config::pointer::child(&path, if visible { &decoded } else { "[redacted]" });
    }
    path
}
