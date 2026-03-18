# nab WASM Provider Example

A site-extractor written in Rust, compiled to a **WIT Component Model** `.wasm`
targeting `wasm32-wasip2`.  Demonstrates the modern nab WASM provider ABI.

## ABI Options

| ABI | Target | Guest code | Status |
|-----|--------|-----------|--------|
| **Component Model** (recommended) | `wasm32-wasip2` | `wit_bindgen::generate!` + `export!` | Preferred |
| Legacy raw C | `wasm32-unknown-unknown` | `extern "C" fn alloc / extract` | Backward-compatible |

nab automatically detects which ABI a `.wasm` file uses: Component Model is tried
first; plain modules fall back to the raw-C ABI automatically.

## Building (Component Model)

### Prerequisites

```sh
# WASI P2 target
rustup target add wasm32-wasip2

# wasm-tools (optional — only needed if you want to inspect or adapt the component)
cargo install wasm-tools
```

### Compile

```sh
cargo build \
    --target wasm32-wasip2 \
    --release \
    --manifest-path examples/wasm_provider/Cargo.toml
```

The compiled component is at:
```
examples/wasm_provider/target/wasm32-wasip2/release/nab_wasm_example.wasm
```

## Installing

```sh
cp examples/wasm_provider/target/wasm32-wasip2/release/nab_wasm_example.wasm \
   /tmp/my-article.wasm

# Install (requires --features wasm-providers build of nab)
nab provider install /tmp/my-article.wasm
nab provider list
nab provider remove generic-article
```

## Guest Interface (WIT Component Model)

The interface is declared in `wit/provider.wit` (workspace root):

```wit
package nab:provider;

interface extractor {
    record article {
        title:   option<string>,
        content: string,
        author:  option<string>,
        date:    option<string>,
    }
    extract: func(url: string, html: string) -> result<article, string>;
}

world provider {
    export extractor;
}
```

Your guest implements it like this:

```rust
wit_bindgen::generate!({
    path: "../../wit/provider.wit",
    world: "provider",
});

struct MyExtractor;

impl exports::nab::provider::extractor::Guest for MyExtractor {
    fn extract(url: String, html: String) -> Result<Article, String> {
        Ok(Article {
            title: Some("My Title".to_string()),
            content: "Extracted content...".to_string(),
            author: None,
            date: None,
        })
    }
}

export!(MyExtractor);
```

## Legacy Raw-C ABI (backward compatible)

Old `.wasm` modules compiled to `wasm32-unknown-unknown` with the raw-C ABI
continue to work unchanged.  Your module must export:

| Export | Type | Description |
|--------|------|-------------|
| `memory` | `(memory 1)` | Linear memory |
| `alloc` | `(i32) -> i32` | Allocate `len` bytes; return pointer |
| `extract` | `(i32, i32, i32, i32) -> i32` | Parse HTML+URL; return JSON pointer (0 = fail) |

The `extract` return value is a pointer to a NUL-terminated JSON string:

```json
{
  "title":        "optional string",
  "content":      "optional markdown or plain text",
  "author":       "optional string",
  "date":         "optional ISO-8601 date",
  "canonical_url":"optional URL (defaults to request URL)"
}
```

## Sandbox guarantees (both ABIs)

- No WASI imports — no filesystem or network access
- Fuel limit: 100 million instructions per extraction call
- Memory limit: 64 MiB per instantiation
- Fresh instance per request (no shared state between calls)
