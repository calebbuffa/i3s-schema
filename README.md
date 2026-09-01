# i3s-schema

Parses the [Esri i3s-spec](https://github.com/Esri/i3s-spec) markdown
documentation (the spec's only source of truth) into **draft-07 JSON Schema
files** that standard tooling can consume.

## Layout

```bash
schema/            generated JSON Schemas - do not edit by hand
  cmn/             common profile (73 types)
  bld/             building scene layer profile (13)
  psl/             point scene layer profile (5)
  pcsl/            point cloud scene layer profile (19)
src/               the parser (not generated)
spec/              git submodule: Esri/i3s-spec (input only)
```

## Usage

```sh
git submodule update --init   # fetch the spec markdown

cargo run -- schema            # regenerate schema/ from spec/docs
cargo run -- schema --check    # CI: exit non-zero if schema/ is stale
cargo run -- validate          # check spec example blocks against schemas
```

Defaults: `--spec-dir spec` (the submodule; its `docs/` subdir is used
automatically) and `--out schema`.
