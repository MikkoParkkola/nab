# nab-loader

LangChain and LlamaIndex document loaders powered by [nab](https://github.com/MikkoParkkola/nab) — a token-optimized HTTP client for LLMs.

## Install

```bash
# Core only (no framework dependencies)
pip install nab-loader

# With LangChain support
pip install "nab-loader[langchain]"

# With LlamaIndex support
pip install "nab-loader[llamaindex]"

# Both frameworks
pip install "nab-loader[all]"
```

Requires the `nab` binary on your PATH:

```bash
brew install mikkoparkkola/tap/nab   # macOS
cargo install nab                     # from source
```

## Usage

### Standalone

```python
from nab_loader import NabLoader

loader = NabLoader()
result = loader.fetch("https://example.com")
print(result.markdown)
print(result.status, result.size, result.time_ms)

# Batch fetch (parallel)
results = loader.fetch_batch([
    "https://example.com",
    "https://python.org",
], parallel=5)
```

### LangChain

```python
from nab_loader import NabWebLoader

loader = NabWebLoader([
    "https://docs.python.org/3/tutorial/",
    "https://rust-lang.org",
])
docs = loader.load()

# Or lazily
for doc in loader.lazy_load():
    print(doc.metadata["source"], len(doc.page_content))
```

### LlamaIndex

```python
from nab_loader import NabWebReader

reader = NabWebReader()
docs = reader.load_data([
    "https://docs.python.org/3/tutorial/",
    "https://rust-lang.org",
])
```

## Why nab?

- Token-optimized markdown output (less noise for LLMs)
- HTTP/2 multiplexing, Brotli/Zstd compression
- Browser cookie integration (access authenticated content)
- Site-specific extractors (Twitter, Reddit, HN, GitHub, YouTube, etc.)
