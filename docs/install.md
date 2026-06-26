# Install

## Requirements

- Rust (stable) and Cargo. Install via [rustup](https://rustup.rs).

## CLI

### From crates.io

```bash
cargo install filetools-rs
```

This installs the `filetools` binary onto your Cargo bin path
(`~/.cargo/bin`). Confirm:

```bash
filetools --help
```

### From source

```bash
git clone https://github.com/kolisachint/filetools
cd filetools
cargo install --path .
```

Or run without installing:

```bash
cargo run --bin filetools -- --help
```

## Library

Add the crate to your project:

```bash
cargo add filetools-rs
```

or in `Cargo.toml`:

```toml
[dependencies]
filetools-rs = "0.1"
```

Minimal use:

```rust
use filetools_rs::{extract, read, write};
use filetools_rs::patch::{Op, Patch};

let bytes = std::fs::read("report.xml")?;
let out = extract("report.xml", &bytes)?;
let idmap = out.idmap.as_ref().expect("writable format");

let patch = Patch {
    patch: vec![Op::Replace {
        path: "/structure/el_8694f8af/text".into(),
        value: "Revenue grew 18%.".into(),
    }],
};
let new_bytes = write(&out.envelope, idmap, &bytes, &patch)?;
std::fs::write("report_v2.xml", new_bytes)?;
# Ok::<(), anyhow::Error>(())
```

## Build & test (contributors)

```bash
cargo build
cargo fmt --all --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```
