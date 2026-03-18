# nab WASM Provider Example

An example site-extractor written in Rust, compiled to `wasm32-unknown-unknown`.
Demonstrates the nab WASM provider ABI.

## Building

```sh
rustup target add wasm32-unknown-unknown
cargo build \
    --target wasm32-unknown-unknown \
    --release \
    --manifest-path examples/wasm_provider/Cargo.toml
```

The compiled module is at:
```
examples/wasm_provider/target/wasm32-unknown-unknown/release/nab_wasm_example.wasm
```

## Installing

```sh
# Copy .wasm + sidecar manifest to a staging area
cp examples/wasm_provider/target/wasm32-unknown-unknown/release/nab_wasm_example.wasm \
   /tmp/generic-article.wasm
cp examples/wasm_provider/manifest.toml /tmp/generic-article.manifest.toml

# Install (requires --features wasm-providers build of nab)
nab provider install /tmp/generic-article.wasm
nab provider list
nab provider remove generic-article
```

## Guest ABI

Your WASM module must export:

| Export | Type | Description |
|--------|------|-------------|
| `memory` | `(memory 1)` | Linear memory |
| `alloc` | `(i32) -> i32` | Allocate `len` bytes; return pointer |
| `extract` | `(i32, i32, i32, i32) -> i32` | Parse HTML+URL; return JSON pointer (0 = fail) |

The host writes HTML bytes at `alloc(html_len)` and URL bytes at `alloc(url_len)`,
then calls `extract(html_ptr, html_len, url_ptr, url_len)`.

The return value is a pointer to a NUL-terminated JSON string conforming to:

```json
{
  "title":        "optional string",
  "content":      "optional markdown or plain text",
  "author":       "optional string",
  "date":         "optional ISO-8601 or human-readable date",
  "canonical_url":"optional URL (defaults to request URL)"
}
```

## Sandbox guarantees

- No WASI — no filesystem or network access
- Fuel limit: 100 million instructions per extraction call
- Memory limit: 64 MiB per instantiation
- Fresh instance per request (no shared state between calls)
