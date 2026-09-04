//! Validate example JSON blocks from the markdown spec against the emitted
//! JSON Schemas.
//!
//! Implements the draft-07 subset the emitter produces: `type`, `required`,
//! `enum`, `items`, `minItems`/`maxItems`, and relative `$ref` resolution.

use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::Path;

/// All loaded schemas: "<profile>/<base>.schema.json" -> schema document.
pub struct SchemaStore {
    files: BTreeMap<String, Value>,
}

impl SchemaStore {
    /// Load every `*.json` under `dir` (recursing one level into profile
    /// directories). Keys are lowercased so lookups are case-insensitive,
    /// matching real filesystem semantics.
    pub fn load(dir: &Path) -> Result<Self> {
        let mut files = BTreeMap::new();
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                let profile = entry.file_name().to_string_lossy().into_owned();
                for e in std::fs::read_dir(&path)?.filter_map(|e| e.ok()) {
                    let p = e.path();
                    if p.extension().is_some_and(|x| x == "json") {
                        let name = e.file_name().to_string_lossy().into_owned();
                        let raw = std::fs::read_to_string(&p)?;
                        let doc: Value = serde_json::from_str(&raw)?;
                        files.insert(format!("{profile}/{name}").to_lowercase(), doc);
                    }
                }
            }
        }
        Ok(Self { files })
    }

    /// Resolve a `$ref` relative to the schema file that contains it
    /// (`current_dir` = "profile/" prefix of that file), mirroring
    /// file-relative JSON Schema resolution. Returns the resolved schema
    /// along with the directory prefix that *its* own bare refs resolve in.
    fn resolve(&self, reference: &str, current_dir: &str) -> Option<(&Value, String)> {
        // `../` and `./` are relative to the referring schema's directory;
        // anything else containing a separator is rooted at the schema tree.
        let key = if reference.starts_with("./") || reference.starts_with("../") {
            normalize_relative(current_dir, reference)?
        } else if reference.contains('/') {
            reference.to_string()
        } else {
            format!("{current_dir}{reference}")
        };
        let value = self.files.get(&key.to_lowercase())?;
        let dir = match key.rfind('/') {
            Some(idx) => key[..=idx].to_string(),
            None => String::new(),
        };
        Some((value, dir))
    }

    /// Validate `instance` against the schema at `<profile>/<file>`.
    /// Returns a list of human-readable error strings (empty = valid).
    pub fn validate(&self, profile: &str, file: &str, instance: &Value) -> Vec<String> {
        let mut errors = Vec::new();
        match self.resolve(file, &format!("{profile}/")) {
            Some((schema, dir)) => self.validate_value(instance, schema, &dir, "$", &mut errors),
            None => errors.push(format!("schema not found: {profile}/{file}")),
        }
        errors
    }
    fn validate_value(
        &self,
        instance: &Value,
        schema: &Value,
        current_dir: &str,
        path: &str,
        errors: &mut Vec<String>,
    ) {
        // $ref: validate against the referenced schema. Refs resolve
        // relative to the *containing* schema file's directory, and bare
        // refs inside the target keep resolving there (file-relative).
        if let Some(reference) = schema.get("$ref").and_then(Value::as_str) {
            match self.resolve(reference, current_dir) {
                Some((target, dir)) => {
                    self.validate_value(instance, target, &dir, path, errors);
                }
                None => errors.push(format!("{path}: unresolved $ref {reference:?}")),
            }
            return;
        }

        // type
        if let Some(expected) = schema.get("type").and_then(Value::as_str) {
            let ok = match expected {
                "object" => instance.is_object(),
                "array" => instance.is_array(),
                "string" => instance.is_string(),
                "boolean" => instance.is_boolean(),
                // JSON makes no strict int/float distinction for validation
                // purposes here; accept any number.
                "number" | "integer" => instance.is_number(),
                other => {
                    errors.push(format!("{path}: unknown schema type {other:?}"));
                    false
                }
            };
            if !ok {
                errors.push(format!(
                    "{path}: expected {expected}, got {}",
                    type_of(instance)
                ));
                return;
            }
        }

        // enum
        if let Some(allowed) = schema.get("enum").and_then(Value::as_array)
            && !allowed.contains(instance)
        {
            errors.push(format!("{path}: value {instance} not in enum"));
        }
        match instance {
            Value::Object(map) => {
                if let Some(required) = schema.get("required").and_then(Value::as_array) {
                    for key in required.iter().filter_map(Value::as_str) {
                        if !map.contains_key(key) {
                            errors.push(format!("{path}: missing required property {key:?}"));
                        }
                    }
                }
                if let Some(properties) = schema.get("properties").and_then(Value::as_object) {
                    for (key, value) in map {
                        if let Some(prop_schema) = properties.get(key) {
                            self.validate_value(
                                value,
                                prop_schema,
                                current_dir,
                                &format!("{path}.{key}"),
                                errors,
                            );
                        }
                    }
                }
            }
            Value::Array(items) => {
                if let Some(min) = schema.get("minItems").and_then(Value::as_u64)
                    && (items.len() as u64) < min
                {
                    errors.push(format!(
                        "{path}: array length {} < minItems {min}",
                        items.len()
                    ));
                }
                if let Some(max) = schema.get("maxItems").and_then(Value::as_u64)
                    && (items.len() as u64) > max
                {
                    errors.push(format!(
                        "{path}: array length {} > maxItems {max}",
                        items.len()
                    ));
                }
                if let Some(item_schema) = schema.get("items") {
                    for (i, item) in items.iter().enumerate() {
                        self.validate_value(
                            item,
                            item_schema,
                            current_dir,
                            &format!("{path}[{i}]"),
                            errors,
                        );
                    }
                }
            }
            _ => {}
        }
    }
}

fn type_of(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

/// Validate every example block in the parsed spec against its type's schema.
/// Returns (checked count, failures) where each failure is
/// "Type[profile] example name: error".
pub fn validate_examples(ir: &crate::parse::Ir, store: &SchemaStore) -> (usize, Vec<String>) {
    let mut checked = 0usize;
    let mut failures = Vec::new();
    for module in ir.modules.values() {
        for entry in &module.types {
            if entry.examples.is_empty() {
                continue;
            }
            let schema_file = format!("{}.schema.json", entry.base_name);
            for example in &entry.examples {
                checked += 1;
                let label = format!("{}[{}] {:?}", entry.name, entry.profile, example.name);
                let Ok(instance) = serde_json::from_str::<Value>(&example.json) else {
                    failures.push(format!("{label}: example is not valid JSON"));
                    continue;
                };
                for error in store.validate(&entry.profile, &schema_file, &instance) {
                    failures.push(format!("{label}: {error}"));
                }
            }
        }
    }
    (checked, failures)
}

/// Resolve a `./` or `../` reference against `current_dir` (a `"profile/"`
/// style prefix), yielding a schema-tree-rooted key. Returns `None` if the
/// reference escapes above the schema tree root.
fn normalize_relative(current_dir: &str, reference: &str) -> Option<String> {
    let mut segments: Vec<&str> = current_dir.split('/').filter(|s| !s.is_empty()).collect();
    for segment in reference.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            other => segments.push(other),
        }
    }
    Some(segments.join("/"))
}

#[cfg(test)]
mod tests {
    use super::normalize_relative;

    #[test]
    fn parent_reference_resolves_across_profiles() {
        assert_eq!(
            normalize_relative("pcsl/", "../cmn/obb.schema.json").as_deref(),
            Some("cmn/obb.schema.json")
        );
    }

    #[test]
    fn same_directory_reference_keeps_profile() {
        assert_eq!(
            normalize_relative("psl/", "./store.schema.json").as_deref(),
            Some("psl/store.schema.json")
        );
    }

    #[test]
    fn escaping_the_schema_root_is_rejected() {
        assert_eq!(normalize_relative("cmn/", "../../etc/passwd"), None);
    }
}
