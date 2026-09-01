//! Parse the Esri [i3s-spec](https://github.com/Esri/i3s-spec) markdown
//! documentation into draft-07 JSON Schema files.
//!
//! The i3s specification has no formal schemas — its normative definition is
//! markdown with type headings, properties tables, and JSON example blocks.
//! This crate converts that into the same draft-07 JSON Schema dialect the
//! moderu/tairu generators consume, so i3s Rust types can be generated with
//! a standard schema-driven codegen pipeline.
//!
//! Layout of the emitted schema tree (`generated/schema/` by default):
//! `schema/<profile>/<base>.schema.json` for every type, where profile is
//! one of `cmn`, `bld`, `psl`, `pcsl`.

pub mod parse;
pub mod schema;
pub mod validate;
