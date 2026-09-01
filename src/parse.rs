//! Parse the i3s-spec markdown type definitions into an intermediate
//! representation, from which draft-07 JSON Schemas are emitted.
//!
//! This crate is deliberately policy-free: it extracts exactly what the
//! markdown says (heading, properties, types, examples) and assigns no Rust
//! names. Downstream consumers (e.g. the `i3s` crate's xtask) own all
//! language-naming decisions.

use anyhow::{Context, Result};
use fancy_regex::Regex as FancyRegex;
use regex::Regex;
use serde::Serialize;
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

static SKIP_README_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)_ReadMe\.md$").unwrap());
static SKIP_ECMA_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"ECMA_ISO8601\.md$").unwrap());

static PROFILE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(cmn|bld|psl|pcsl)\.md$").unwrap());
static BASE_NAME_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.(cmn|bld|psl|pcsl)\.md$").unwrap());
static VERSION_DIR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+\.\d+$").unwrap());

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TypeDesc {
    Reference {
        name: String,
        base_name: String,
        profile: String,
    },
    Array {
        element: Box<TypeDesc>,
    },
    FixedArray {
        element_type: String,
        size: u32,
    },
    Primitive {
        #[serde(rename = "type")]
        type_name: String,
    },
    /// Comma-separated primitive union, e.g. "string, number".
    Union {
        types: Vec<String>,
    },
    /// Empty or unrecognized type cell: unconstrained (any JSON value).
    Unknown,
}

#[derive(Debug, Serialize)]
pub struct Property {
    pub name: String,
    #[serde(rename = "type")]
    pub type_desc: TypeDesc,
    pub required: bool,
    pub description: String,
    #[serde(rename = "enum_values")]
    pub enum_values: Option<Vec<String>>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
}

#[derive(Debug, Serialize)]
pub struct Example {
    pub name: String,
    pub json: String,
}

#[derive(Debug, Serialize)]
pub struct TypeEntry {
    /// The type heading from the markdown, verbatim (e.g. `3DSceneLayer`).
    pub name: String,
    /// Lowercased filename stem without the profile suffix.
    pub base_name: String,
    pub profile: String,
    pub module: String,
    pub description: String,
    pub properties: Vec<Property>,
    pub examples: Vec<Example>,
    pub source_file: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub deprecated: bool,
}

#[derive(Debug, Serialize)]
pub struct ModuleData {
    pub types: Vec<TypeEntry>,
}

#[derive(Debug, Serialize)]
pub struct Ir {
    pub version: String,
    /// Sorted by module name (matches Python's `sorted(types_by_module.items())`).
    pub modules: BTreeMap<String, ModuleData>,
}

fn parse_type_column(type_str: &str) -> TypeDesc {
    let type_str = type_str.trim();

    static REF_ARRAY_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\[([^\]]+)\]\(([^)]+)\)\[(.*?)\]$").unwrap());
    static REF_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\[([^\]]+)\]\(([^)]+)\)$").unwrap());
    static PRIM_CARD_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(string|integer|number|boolean)\[(.*?)\]$").unwrap());

    // Reference with cardinality suffix: [typeName](file.md)[], [1], [1:2], [1:]...
    if let Some(m) = REF_ARRAY_RE.captures(type_str) {
        let (ref_name, ref_file) = (m[1].to_string(), m[2].to_string());
        let ref_profile = parse_profile(&ref_file).unwrap_or_else(|| "cmn".to_string());
        let ref_base = parse_base_name(Path::new(&ref_file));
        return TypeDesc::Array {
            element: Box::new(TypeDesc::Reference {
                name: ref_name,
                base_name: ref_base,
                profile: ref_profile,
            }),
        };
    }

    // Reference: [typeName](file.md)
    if let Some(m) = REF_RE.captures(type_str) {
        let (ref_name, ref_file) = (m[1].to_string(), m[2].to_string());
        let ref_profile = parse_profile(&ref_file).unwrap_or_else(|| "cmn".to_string());
        let ref_base = parse_base_name(Path::new(&ref_file));
        return TypeDesc::Reference {
            name: ref_name,
            base_name: ref_base,
            profile: ref_profile,
        };
    }

    // Primitive with cardinality suffix: number[3] fixed, number[]/[:256]/[1:2] dynamic.
    if let Some(m) = PRIM_CARD_RE.captures(type_str) {
        let primitive = m[1].to_string();
        let cardinality = m[2].trim();
        if !cardinality.is_empty() && cardinality.chars().all(|c| c.is_ascii_digit()) {
            return TypeDesc::FixedArray {
                element_type: primitive,
                size: cardinality.parse().unwrap_or(0),
            };
        }
        return TypeDesc::Array {
            element: Box::new(TypeDesc::Primitive {
                type_name: primitive,
            }),
        };
    }

    // Primitive: string, integer, number, boolean
    if matches!(type_str, "string" | "integer" | "number" | "boolean") {
        return TypeDesc::Primitive {
            type_name: type_str.to_string(),
        };
    }

    // Union of primitives: e.g. "string, number"
    let parts: Vec<String> = type_str.split(',').map(|s| s.trim().to_string()).collect();
    if parts.len() > 1
        && parts
            .iter()
            .all(|p| matches!(p.as_str(), "string" | "integer" | "number" | "boolean"))
    {
        return TypeDesc::Union { types: parts };
    }

    // The spec uses a bare `[]` for "array of unspecified element type".
    if type_str == "[]" {
        return TypeDesc::Array {
            element: Box::new(TypeDesc::Unknown),
        };
    }

    // Empty or unrecognized: unconstrained. (Schema emission makes this
    // visible via a warning instead of silently coercing to `string`.)
    TypeDesc::Unknown
}

fn extract_enum_values(description: &str) -> Option<Vec<String>> {
    static LI_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<li>`([^`]+)`").unwrap());
    let matches: Vec<String> = LI_RE
        .captures_iter(description)
        .map(|m| m[1].to_string())
        .collect();
    if matches.is_empty() {
        None
    } else {
        Some(matches)
    }
}

fn is_deprecated(description: &str) -> bool {
    static DEPRECATED_BOLD: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"\*\*Deprecated").unwrap());
    static DEPRECATED_IN: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\bdeprecated\s+(?:in|with|for)\s+\d+\.\d+").unwrap());
    static DEPRECATED_END: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)\bdeprecated\.?\s*$").unwrap());
    DEPRECATED_BOLD.is_match(description)
        || DEPRECATED_IN.is_match(description)
        || DEPRECATED_END.is_match(description)
}

/// Equivalent of Python's `re.sub(r"<[^>]+>", "", s).strip()`.
fn strip_html_tags(s: &str) -> String {
    static HTML_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<[^>]+>").unwrap());
    HTML_RE.replace_all(s, "").trim().to_string()
}

fn parse_properties_table(
    content: &str,
    source_file: &str,
    warnings: &mut Vec<String>,
) -> Vec<Property> {
    let mut props = Vec::new();

    static PROPS_HEADING_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"### Properties\s*\n").unwrap());
    let Some(m) = PROPS_HEADING_RE.find(content) else {
        return props;
    };

    let rest = &content[m.end()..];
    let mut table_lines: Vec<&str> = Vec::new();
    let mut found_separator = false;

    for line in rest.lines() {
        let stripped = line.trim();
        if stripped.is_empty() {
            if found_separator {
                break; // End of table
            }
            continue;
        }
        if !found_separator && stripped.starts_with('|') && stripped.contains("---") {
            found_separator = true;
            continue;
        }
        if !found_separator && stripped.starts_with('|') {
            // Header row (before the separator line)
            continue;
        }
        if found_separator && stripped.starts_with('|') {
            table_lines.push(stripped);
        } else if found_separator {
            break; // End of table
        }
    }

    for line in table_lines {
        let mut cells: Vec<&str> = line.split('|').map(|c| c.trim()).collect();
        if !cells.is_empty() && cells[0].is_empty() {
            cells.remove(0);
        }
        if !cells.is_empty() && cells[cells.len() - 1].is_empty() {
            cells.pop();
        }
        if cells.len() < 3 {
            continue;
        }

        let name_cell = cells[0].trim();
        let type_cell = cells[1].trim();
        let desc_cell = cells[2].trim();

        // Check if required (bold)
        let required = name_cell.starts_with("**") && name_cell.ends_with("**");
        let name = name_cell.trim_matches('*').trim();

        if name.is_empty() {
            continue;
        }

        let type_desc = parse_type_column(type_cell);
        if matches!(type_desc, TypeDesc::Unknown) {
            warnings.push(format!(
                "unparsed type expression {:?} for property {:?} in {}: treated as unconstrained",
                type_cell, name, source_file
            ));
        }
        let enum_values = extract_enum_values(desc_cell);
        let clean_desc = strip_html_tags(desc_cell);

        props.push(Property {
            name: name.to_string(),
            type_desc,
            required,
            description: clean_desc,
            enum_values,
            deprecated: is_deprecated(desc_cell),
        });
    }

    props
}

fn parse_type_name(content: &str) -> Option<String> {
    static HEADING_RE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^#\s+(.+?)(?:\s+\[.*\])?\s*$").unwrap());
    let first_line = content.lines().next()?;
    HEADING_RE
        .captures(first_line)
        .map(|m| m[1].trim().to_string())
}

fn parse_description(content: &str) -> String {
    let mut desc_lines: Vec<&str> = Vec::new();
    let mut started = false;
    for line in content.lines().skip(1) {
        // Skip heading
        let stripped = line.trim();
        if stripped.starts_with("### ") || stripped.starts_with("## ") {
            break;
        }
        if !stripped.is_empty() {
            started = true;
        }
        if started {
            desc_lines.push(stripped);
        }
    }
    desc_lines.join(" ").trim().to_string()
}

fn parse_examples(content: &str) -> Vec<Example> {
    static EXAMPLE_RE: LazyLock<FancyRegex> = LazyLock::new(|| {
        FancyRegex::new(r"(?s)####\s+Example:\s*(.+?)\s*\n.*?```json\s*\n(.*?)```").unwrap()
    });
    let mut examples = Vec::new();
    for m in EXAMPLE_RE.captures_iter(content).flatten() {
        examples.push(Example {
            name: m[1].trim().to_string(),
            json: m[2].trim().to_string(),
        });
    }
    examples
}

fn should_skip(filename: &str) -> bool {
    SKIP_README_RE.is_match(filename) || SKIP_ECMA_RE.is_match(filename)
}

fn parse_profile(filename: &str) -> Option<String> {
    PROFILE_RE
        .captures(filename)
        .map(|m| m[1].to_ascii_lowercase())
}

fn parse_base_name(path: &Path) -> String {
    let filename = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    BASE_NAME_RE.replace(&filename, "").trim().to_string()
}

pub fn parse_md_file(
    spec_root: &Path,
    filepath: &Path,
    warnings: &mut Vec<String>,
) -> Result<Option<TypeEntry>> {
    let filename = filepath
        .file_name()
        .context("file has no name")?
        .to_string_lossy()
        .into_owned();
    if should_skip(&filename) {
        return Ok(None);
    }

    let Some(profile) = parse_profile(&filename) else {
        return Ok(None);
    };

    let base_name = parse_base_name(filepath);
    // Python reads with encoding="utf-8", errors="replace"; replicate with
    // lossy decoding (at least one spec file is not valid UTF-8).
    let bytes =
        std::fs::read(filepath).with_context(|| format!("reading {}", filepath.display()))?;
    let content = String::from_utf8_lossy(&bytes).into_owned();
    // Python's read_text() uses universal newlines: translate \r\n and \r
    // to \n (repo files are checked out with CRLF on Windows).
    let content = content.replace("\r\n", "\n").replace('\r', "\n");

    let Some(type_name) = parse_type_name(&content) else {
        return Ok(None);
    };

    let source_file = filepath
        .strip_prefix(spec_root)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| filepath.to_string_lossy().into_owned());

    let properties = parse_properties_table(&content, &source_file, warnings);
    let description = parse_description(&content);
    let examples = parse_examples(&content);
    let module = profile.clone();

    Ok(Some(TypeEntry {
        deprecated: is_deprecated(&description),
        name: type_name,
        base_name,
        profile,
        module,
        description,
        properties,
        examples,
        source_file,
    }))
}

/// Maps `"<base_name>:<profile>"` -> sorted list of spec version dirs in
/// which the type appears (e.g. `["1.7", "1.8", "1.10"]`).
pub type VersionIndex = BTreeMap<String, Vec<String>>;

pub fn parse_spec(spec_dir: &Path) -> Result<(Ir, VersionIndex, Vec<String>)> {
    // Auto-discover all version directories (e.g. 1.6, 1.7, ..., 2.0, 2.1).
    // Sort newest-first so the latest definition of each type wins.
    let mut version_dirs: Vec<(u32, u32, PathBuf)> = Vec::new();
    for entry in std::fs::read_dir(spec_dir)
        .with_context(|| format!("reading spec dir {}", spec_dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        if path.is_dir() && VERSION_DIR_RE.is_match(&name) {
            let (maj, min) = name
                .split_once('.')
                .and_then(|(a, b)| match (a.parse(), b.parse()) {
                    (Ok(a), Ok(b)) => Some((a, b)),
                    _ => None,
                })
                .with_context(|| format!("bad version dir name: {name}"))?;
            version_dirs.push((maj, min, path));
        }
    }
    version_dirs.sort_by_key(|(maj, min, _)| Reverse((*maj, *min)));

    let mut types_by_module: BTreeMap<String, Vec<TypeEntry>> = BTreeMap::new();
    let mut seen_types: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut version_index: VersionIndex = BTreeMap::new();
    let mut warnings: Vec<String> = Vec::new();

    for (maj, min, version_dir) in &version_dirs {
        let version = format!("{maj}.{min}");
        let mut md_files: Vec<PathBuf> = std::fs::read_dir(version_dir)
            .with_context(|| format!("reading {}", version_dir.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("md"))
            })
            .collect();
        md_files.sort();

        for md_file in md_files {
            let Some(parsed) = parse_md_file(
                spec_dir.parent().unwrap_or(spec_dir),
                &md_file,
                &mut warnings,
            )?
            else {
                continue;
            };
            let dedup_key = format!("{}:{}", parsed.base_name.to_lowercase(), parsed.profile);
            version_index
                .entry(dedup_key.clone())
                .or_default()
                .push(version.clone());
            if seen_types.contains(&dedup_key) {
                continue;
            }
            seen_types.insert(dedup_key);
            types_by_module
                .entry(parsed.module.clone())
                .or_default()
                .push(parsed);
        }
    }

    let versions_found: Vec<String> = version_dirs
        .iter()
        .map(|(_, _, p)| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();

    Ok((
        Ir {
            version: versions_found.join("+"),
            modules: types_by_module
                .into_iter()
                .map(|(name, types)| (name, ModuleData { types }))
                .collect(),
        },
        version_index,
        warnings,
    ))
}
