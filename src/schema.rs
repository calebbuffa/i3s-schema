//! Convert the parsed markdown IR into draft-07 JSON Schema files that the
//! moderu/tairu-style generator (`schema.rs` + `SchemaCache`) can consume
//! directly.
//!
//! Layout: one schema file per type at `<out>/<profile>/<base_name>.schema.json`.
//! References use relative `$ref`s: bare filenames within the same profile,
//! `<profile>/<base>.schema.json` across profiles (resolved via the
//! generator's schema search paths).

use crate::parse::{Ir, TypeDesc, TypeEntry, VersionIndex};
use anyhow::Result;
use regex::Regex;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::LazyLock;

/// Normalize whitespace and markup artifacts inherited from the upstream
/// markdown. This is *presentation* cleanup only — names, enum values and
/// wording are never altered:
///
/// - HTML tags (`<sup>n</sup>`) are removed, matching how property
///   descriptions are already treated.
/// - The spec uses triple-backtick delimiters for inline code spans and
///   code fences (```` ```code``` ````); after multi-line descriptions are
///   flattened these runs become stray ``/``` sequences. Collapsing every
///   run of 2+ backticks to one turns them back into proper single-backtick
///   code spans. (The raw spec contains no *intentional* 2+-backtick runs,
///   so this is lossless.)
/// - Missing whitespace after sentence punctuation is inserted
///   (`type.Possible` -> `type. Possible`, `fulfills.{point` ->
///   `fulfills. {point`).
/// - Runs of spaces collapse to one.
pub fn clean_description(s: &str) -> String {
    static HTML: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
    static BACKTICK_RUN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"`{2,}").unwrap());
    static PUNCT_UPPER: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r#"([\w)'"'\]])\.([A-Z{])"#).unwrap());
    static PUNCT_TICK: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"([a-z])([.,])`([A-Za-z])").unwrap());
    static COLON_TICK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([a-z]):`").unwrap());
    static MULTI_SPACE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"  +").unwrap());

    let s = HTML.replace_all(s, "");
    let s = BACKTICK_RUN.replace_all(&s, "`");
    let s = PUNCT_UPPER.replace_all(&s, "$1. $2");
    let s = PUNCT_TICK.replace_all(&s, "$1$2 `$3");
    let s = COLON_TICK.replace_all(&s, "$1: `");
    let s = MULTI_SPACE.replace_all(&s, " ");
    s.trim().to_owned()
}

/// Render one type as a draft-07 JSON Schema object.
/// `canonical` maps lowercase base names -> the canonical base name used for
/// schema filenames, so `$ref`s written from markdown links resolve
/// case-insensitively on every platform.
pub fn type_schema(
    entry: &TypeEntry,
    versions: &[String],
    canonical: &std::collections::HashMap<String, String>,
) -> Value {
    let mut root = Map::new();
    root.insert(
        "$schema".into(),
        Value::String("http://json-schema.org/draft-07/schema#".into()),
    );
    // The title is the markdown heading, verbatim — this crate makes no
    // naming decisions. Consumers derive language-specific names from it.
    root.insert("title".into(), Value::String(entry.name.clone()));
    if !entry.description.is_empty() {
        root.insert(
            "description".into(),
            Value::String(clean_description(&entry.description)),
        );
    }
    root.insert("type".into(), Value::String("object".into()));

    let mut properties = Map::new();
    let mut required = Vec::new();
    let mut additional_properties: Option<Value> = None;
    for prop in &entry.properties {
        // `(identifier)` marks a map-typed property: the keys are arbitrary
        // unique identifiers, so it becomes `additionalProperties` rather
        // than a named property (and is never `required`).
        if prop.name == "(identifier)" || prop.name == "{identifier}" {
            additional_properties =
                Some(type_desc_schema(&prop.type_desc, &entry.profile, canonical));
            continue;
        }

        let mut schema = type_desc_schema(&prop.type_desc, &entry.profile, canonical);
        let obj = schema.as_object_mut().unwrap();

        // Enumerations constrain array *elements*.
        if let Some(enum_values) = &prop.enum_values {
            let enum_val = Value::Array(enum_values.iter().cloned().map(Value::String).collect());
            let is_array = obj.get("type").and_then(Value::as_str) == Some("array");
            if is_array {
                let items = obj
                    .entry("items")
                    .or_insert_with(|| Value::Object(Map::new()));
                items
                    .as_object_mut()
                    .unwrap()
                    .insert("enum".into(), enum_val);
            } else {
                obj.insert("enum".into(), enum_val);
            }
        }
        if !prop.description.is_empty() {
            obj.insert(
                "description".into(),
                Value::String(clean_description(&prop.description)),
            );
        }
        if prop.deprecated {
            obj.insert("deprecated".into(), Value::Bool(true));
        }
        properties.insert(prop.name.clone(), schema);

        if prop.required {
            required.push(Value::String(prop.name.clone()));
        }
    }
    root.insert("properties".into(), Value::Object(properties));
    if !required.is_empty() {
        root.insert("required".into(), Value::Array(required));
    }
    if let Some(ap) = additional_properties {
        root.insert("additionalProperties".into(), ap);
    }
    if entry.deprecated {
        root.insert("deprecated".into(), Value::Bool(true));
    }
    if !versions.is_empty() {
        root.insert(
            "x-i3s-versions".into(),
            Value::Array(versions.iter().cloned().map(Value::String).collect()),
        );
    }
    Value::Object(root)
}

/// Map of lowercase base name -> canonical base name (as used in filenames),
/// built once so `$ref`s resolve case-insensitively on any platform.
pub fn canonical_names(ir: &Ir) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for module in ir.modules.values() {
        for entry in &module.types {
            map.insert(
                format!("{}:{}", entry.base_name.to_lowercase(), entry.profile),
                entry.base_name.clone(),
            );
            // Bare-name lookup defaults to the cmn definition when present.
            if entry.profile == "cmn" {
                map.insert(entry.base_name.to_lowercase(), entry.base_name.clone());
            }
        }
    }
    map
}

fn type_desc_schema(
    desc: &TypeDesc,
    own_profile: &str,
    canonical: &std::collections::HashMap<String, String>,
) -> Value {
    // Canonicalize a referenced base name; falls back to the raw name when
    // the target isn't in the parsed set (emitter warns separately).
    let canon = |base: &str, profile: &str| -> String {
        canonical
            .get(&format!("{}:{}", base.to_lowercase(), profile))
            .cloned()
            .unwrap_or_else(|| base.to_string())
    };

    match desc {
        TypeDesc::Primitive { type_name } => {
            let mut m = Map::new();
            m.insert("type".into(), Value::String(type_name.clone()));
            Value::Object(m)
        }
        TypeDesc::Union { types } => {
            let mut m = Map::new();
            m.insert(
                "type".into(),
                Value::Array(types.iter().cloned().map(Value::String).collect()),
            );
            Value::Object(m)
        }
        TypeDesc::Unknown => Value::Object(Map::new()), // unconstrained: {}
        TypeDesc::FixedArray { element_type, size } => {
            let mut items = Map::new();
            items.insert("type".into(), Value::String(element_type.clone()));
            let mut m = Map::new();
            m.insert("type".into(), Value::String("array".into()));
            m.insert("items".into(), Value::Object(items));
            m.insert("minItems".into(), Value::Number((*size).into()));
            m.insert("maxItems".into(), Value::Number((*size).into()));
            Value::Object(m)
        }
        TypeDesc::Array { element } => {
            let mut m = Map::new();
            m.insert("type".into(), Value::String("array".into()));
            m.insert(
                "items".into(),
                type_desc_schema(element, own_profile, canonical),
            );
            Value::Object(m)
        }
        TypeDesc::Reference {
            base_name, profile, ..
        } => {
            // Same-profile refs are bare filenames (resolve context-relatively
            // in the generator); cross-profile refs include the profile dir and
            // resolve via the generator's schema search paths.
            let target = if profile == own_profile {
                format!("{}.schema.json", canon(base_name, profile))
            } else {
                format!("{}/{}.schema.json", profile, canon(base_name, profile))
            };
            let mut m = Map::new();
            m.insert("$ref".into(), Value::String(target));
            Value::Object(m)
        }
    }
}

/// Render all types to an in-memory map of relative path -> JSON text.
/// Keys are sorted (BTreeMap) so output order is deterministic.
pub fn render_schemas(
    ir: &Ir,
    versions: &VersionIndex,
) -> std::collections::BTreeMap<String, String> {
    let canonical = canonical_names(ir);
    let mut files = std::collections::BTreeMap::new();
    for module in ir.modules.values() {
        for entry in &module.types {
            let key = format!("{}:{}", entry.base_name.to_lowercase(), entry.profile);
            let empty = Vec::new();
            let type_versions = versions.get(&key).unwrap_or(&empty);
            let doc = type_schema(entry, type_versions, &canonical);
            let mut buf = Vec::new();
            let formatter = serde_json::ser::PrettyFormatter::with_indent(b"  ");
            let mut ser = serde_json::Serializer::with_formatter(&mut buf, formatter);
            serde::Serialize::serialize(&doc, &mut ser).expect("serialize schema");
            let text = String::from_utf8(buf).expect("utf8") + "\n";
            let path = format!("{}/{}.schema.json", entry.profile, entry.base_name);
            files.insert(path, text);
        }
    }
    files
}

/// Write rendered schemas to `out_dir`, removing stale `.schema.json` files.
pub fn write_schemas(
    out_dir: &Path,
    files: &std::collections::BTreeMap<String, String>,
) -> Result<()> {
    std::fs::create_dir_all(out_dir)?;
    // Remove stale schema files from previous runs.
    for profile_dir in ["cmn", "bld", "psl", "pcsl"] {
        let dir = out_dir.join(profile_dir);
        if dir.is_dir() {
            for e in std::fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "json") {
                    std::fs::remove_file(&p)?;
                }
            }
        }
    }
    for (rel, text) in files {
        let path = out_dir.join(rel.replace('/', "\\"));
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, text)?;
    }
    Ok(())
}
