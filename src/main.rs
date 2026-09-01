//! i3s-schema CLI: parse the i3s-spec markdown into draft-07 JSON Schema
//! files and validate the spec's example blocks against them.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use i3s_schema::{parse, schema, validate};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    name = "i3s-schema",
    about = "Parse the i3s-spec markdown documentation into draft-07 JSON Schema files"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Parse the markdown spec and emit draft-07 JSON Schema files.
    Schema {
        /// Path to the i3s-spec checkout (uses its `docs/` subdirectory).
        #[arg(long, default_value = "spec")]
        spec_dir: PathBuf,
        /// Output directory for the schema tree.
        #[arg(long, default_value = "schema")]
        out: PathBuf,
        /// Exit non-zero if the output would change (no write).
        #[arg(long)]
        check: bool,
    },
    /// Validate example JSON blocks from the spec against their schemas.
    Validate {
        /// Path to the i3s-spec checkout (uses its `docs/` subdirectory).
        #[arg(long, default_value = "spec")]
        spec_dir: PathBuf,
        /// Schema tree directory to validate against.
        #[arg(long, default_value = "schema")]
        out: PathBuf,
        /// Exit non-zero on any example/schema mismatch (default: report
        /// only; upstream spec examples contain known inconsistencies).
        #[arg(long)]
        strict: bool,
    },
}

/// Accept either the spec checkout root (`spec/`) or its docs directory
/// (`spec/docs/`) and return the docs directory containing version folders.
fn resolve_docs_dir(spec_dir: &Path) -> PathBuf {
    if spec_dir.join("docs").is_dir() {
        spec_dir.join("docs")
    } else {
        spec_dir.to_path_buf()
    }
}

fn cmd_schema(spec_dir: PathBuf, out: PathBuf, check: bool) -> Result<()> {
    let docs = resolve_docs_dir(&spec_dir);
    let (ir, versions, warnings) = parse::parse_spec(&docs)
        .with_context(|| format!("parsing spec from {}", docs.display()))?;
    for w in &warnings {
        eprintln!("warning: {w}");
    }
    let files = schema::render_schemas(&ir, &versions);

    if check {
        let mut stale = false;
        // Every rendered file must exist with identical content...
        for (rel, text) in &files {
            let path = out.join(rel.replace('/', "\\"));
            let existing = std::fs::read_to_string(&path).unwrap_or_default();
            if existing != *text {
                eprintln!("out of date: {}", path.display());
                stale = true;
            }
        }
        // ...and there must be no extra schema files on disk.
        for profile_dir in ["cmn", "bld", "psl", "pcsl"] {
            let dir = out.join(profile_dir);
            if !dir.is_dir() {
                continue;
            }
            for e in std::fs::read_dir(&dir)?.filter_map(|e| e.ok()) {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "json") {
                    let rel = format!(
                        "{}/{}",
                        profile_dir,
                        p.file_name().unwrap().to_string_lossy()
                    );
                    if !files.contains_key(&rel) {
                        eprintln!("stale file: {}", p.display());
                        stale = true;
                    }
                }
            }
        }
        if stale {
            anyhow::bail!(
                "check failed: schemas under {} are out of date \
                 (run i3s-schema schema without --check)",
                out.display()
            );
        }
        eprintln!("check ok: {} schemas up to date", files.len());
    } else {
        schema::write_schemas(&out, &files)?;
        eprintln!("wrote {} schemas to {}", files.len(), out.display());
    }
    Ok(())
}

fn cmd_validate(spec_dir: PathBuf, out: PathBuf, strict: bool) -> Result<()> {
    let docs = resolve_docs_dir(&spec_dir);
    let (ir, _, _) = parse::parse_spec(&docs)
        .with_context(|| format!("parsing spec from {}", docs.display()))?;
    let store = validate::SchemaStore::load(&out)
        .with_context(|| format!("loading schemas from {}", out.display()))?;
    let (checked, failures) = validate::validate_examples(&ir, &store);
    for f in &failures {
        eprintln!("MISMATCH: {f}");
    }
    eprintln!(
        "validated {checked} examples, {} mismatches",
        failures.len()
    );
    if !failures.is_empty() {
        if strict {
            anyhow::bail!("validation failed (--strict)");
        }
        eprintln!(
            "note: mismatches are inconsistencies in the upstream spec examples; \
             pass --strict to treat them as errors"
        );
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Schema {
            spec_dir,
            out,
            check,
        } => cmd_schema(spec_dir, out, check),
        Command::Validate {
            spec_dir,
            out,
            strict,
        } => cmd_validate(spec_dir, out, strict),
    }
}
