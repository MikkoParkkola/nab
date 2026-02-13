# Content Handler Architecture: PDF Support & Extensibility

> Architecture design for `nab` content-type-aware response handling.
> Introduces a `ContentHandler` trait, PDF-to-Markdown pipeline via `pdfium-render`,
> and extension points for future `nab submit` (form POST) and `nab login` flows.

## Problem Statement

Today `nab fetch` treats all responses as HTML. It calls `html_to_markdown()` unconditionally
(`src/main.rs:804`), which silently corrupts binary content (PDF, images, archives) into garbage
markdown. There is no Content-Type routing.

**10x outcome**: Fetching a PDF URL should produce clean markdown with tables preserved --
no user intervention, no external tools, ~10ms/page.

## Design Constraints

| Constraint | Value | Rationale |
|------------|-------|-----------|
| License | MIT (nab) + Apache-2.0 (pdfium-render) | Compatible |
| Binary size | Behind feature flag | pdfium adds ~4MB static |
| Latency | ~10ms/page | Competitive with `pdftotext` |
| Rust edition | 2021, MSRV 1.93 | Match existing Cargo.toml |
| No new async runtime | Use existing tokio | pdfium is sync, run in `spawn_blocking` |

## Architecture Overview

```
                        ┌──────────────────────┐
                        │  Response (reqwest)   │
                        │  Content-Type header  │
                        │  + body bytes         │
                        └──────────┬───────────┘
                                   │
                        ┌──────────▼───────────┐
                        │  ContentRouter       │
                        │  (Content-Type →     │
                        │   handler dispatch)  │
                        └──────────┬───────────┘
                                   │
              ┌────────────────────┼────────────────────┐
              │                    │                     │
    ┌─────────▼────────┐ ┌────────▼────────┐ ┌─────────▼────────┐
    │  HtmlHandler     │ │  PdfHandler     │ │  PlainHandler    │
    │  (existing logic)│ │  (pdfium-render)│ │  (passthrough)   │
    │                  │ │                  │ │                  │
    │  html2md::parse  │ │  extract chars  │ │  return as-is    │
    │  + boilerplate   │ │  → line recon   │ │  (text/*, json)  │
    │    filtering     │ │  → table detect │ │                  │
    └──────────────────┘ │  → md render    │ └──────────────────┘
                         └─────────────────┘
```

## 1. ContentHandler Trait

```rust
// src/content/mod.rs

use anyhow::Result;

/// Metadata about the conversion result
#[derive(Debug, Clone)]
pub struct ConversionResult {
    /// The converted markdown content
    pub markdown: String,
    /// Number of pages (for paginated formats like PDF)
    pub page_count: Option<usize>,
    /// Original content type
    pub content_type: String,
    /// Conversion time in milliseconds
    pub elapsed_ms: f64,
}

/// Trait for converting response bytes into markdown.
///
/// Implementations are stateless and sync. The router runs them
/// inside `tokio::task::spawn_blocking` when needed.
pub trait ContentHandler: Send + Sync {
    /// MIME types this handler supports (e.g., ["text/html", "application/xhtml+xml"]).
    fn supported_types(&self) -> &[&str];

    /// Convert raw response bytes to markdown.
    /// `content_type` is the full Content-Type header value (may include charset).
    fn to_markdown(&self, bytes: &[u8], content_type: &str) -> Result<ConversionResult>;
}
```

**Why stateless + sync**: pdfium-render is inherently sync (FFI to C library). Keeping handlers
sync avoids the async-trait overhead and lets the router decide whether to `spawn_blocking`.
HTML conversion via `html2md` is also sync. This is the simplest correct design.

**Why `&[u8]` not `&str`**: PDF is binary. HTML could be non-UTF8 (reqwest handles charset,
but raw bytes are more general). The handler does its own decoding.

## 2. Module Layout

```
src/
├── content/
│   ├── mod.rs          # ContentHandler trait + ContentRouter + re-exports
│   ├── html.rs         # HtmlHandler (wraps existing html_to_markdown logic)
│   ├── pdf.rs          # PdfHandler (pdfium-render, behind feature flag)
│   ├── plain.rs        # PlainHandler (text/plain, application/json passthrough)
│   └── table.rs        # Table detection algorithm (shared by PDF, future XLSX)
├── lib.rs              # Add: pub mod content;
├── main.rs             # Modify: cmd_fetch uses ContentRouter instead of html_to_markdown
└── ...existing modules
```

**File hygiene**: `html_to_markdown` and `is_boilerplate` move from `main.rs:804-834` into
`content/html.rs`. The original functions become thin wrappers during migration, then get removed.

## 3. ContentRouter

```rust
// src/content/mod.rs (continued)

pub mod html;
pub mod plain;
#[cfg(feature = "pdf")]
pub mod pdf;
pub mod table;

use std::time::Instant;

/// Routes response bytes to the appropriate content handler based on Content-Type.
pub struct ContentRouter {
    handlers: Vec<Box<dyn ContentHandler>>,
}

impl ContentRouter {
    pub fn new() -> Self {
        let mut handlers: Vec<Box<dyn ContentHandler>> = vec![
            Box::new(html::HtmlHandler),
            Box::new(plain::PlainHandler),
        ];

        #[cfg(feature = "pdf")]
        handlers.insert(0, Box::new(pdf::PdfHandler::new()));

        Self { handlers }
    }

    /// Find handler for a Content-Type and convert.
    /// Falls back to PlainHandler if no specific handler matches.
    pub fn convert(&self, bytes: &[u8], content_type: &str) -> anyhow::Result<ConversionResult> {
        let mime = content_type
            .split(';')
            .next()
            .unwrap_or(content_type)
            .trim()
            .to_lowercase();

        for handler in &self.handlers {
            if handler.supported_types().iter().any(|t| *t == mime) {
                return handler.to_markdown(bytes, content_type);
            }
        }

        // Fallback: if it looks like HTML (common for missing Content-Type), use HTML handler
        if bytes.starts_with(b"<!") || bytes.starts_with(b"<html") || bytes.starts_with(b"<HTML") {
            return self.handlers
                .iter()
                .find(|h| h.supported_types().contains(&"text/html"))
                .expect("HtmlHandler always registered")
                .to_markdown(bytes, "text/html");
        }

        // Ultimate fallback: plain text
        plain::PlainHandler.to_markdown(bytes, content_type)
    }
}

impl Default for ContentRouter {
    fn default() -> Self {
        Self::new()
    }
}
```

**Dispatch is O(n) over handlers**: With 3-5 handlers this is negligible. If it ever grows to 20+,
switch to a `HashMap<String, usize>` index. Not now (Rams #10).

## 4. PDF Pipeline: pdfium -> positions -> table detection -> markdown

### 4.1 Character Extraction

```rust
// src/content/pdf.rs

use anyhow::Result;
use pdfium_render::prelude::*;
use super::{ContentHandler, ConversionResult};
use super::table::{detect_tables, Table};

/// A positioned character from PDF extraction
#[derive(Debug, Clone)]
struct PdfChar {
    ch: char,
    x: f32,       // left edge in points (1pt = 1/72 inch)
    y: f32,       // baseline in points (bottom-up coordinate system)
    width: f32,
    height: f32,  // font size approximation
    page: usize,
}

/// A reconstructed text line
#[derive(Debug, Clone)]
struct TextLine {
    text: String,
    x: f32,
    y: f32,
    chars: Vec<PdfChar>,
    page: usize,
}

pub struct PdfHandler {
    // pdfium-render uses a static binding; no per-instance state needed
}

impl PdfHandler {
    pub fn new() -> Self {
        Self {}
    }

    /// Extract all characters with positions from a PDF document
    fn extract_chars(bytes: &[u8]) -> Result<(Vec<PdfChar>, usize)> {
        let pdfium = Pdfium::default();
        let doc = pdfium.load_pdf_from_byte_slice(bytes, None)?;
        let page_count = doc.pages().len();
        let mut chars = Vec::new();

        for (page_idx, page) in doc.pages().iter().enumerate() {
            let text = page.text()?;
            for (char_idx, ch) in text.chars().enumerate() {
                if let Ok(rect) = text.char_rect(char_idx) {
                    chars.push(PdfChar {
                        ch: ch.into(),
                        x: rect.left.value,
                        y: rect.bottom.value,
                        width: (rect.right.value - rect.left.value).abs(),
                        height: (rect.top.value - rect.bottom.value).abs(),
                        page: page_idx,
                    });
                }
            }
        }

        Ok((chars, page_count))
    }

    /// Reconstruct text lines from positioned characters.
    ///
    /// Algorithm:
    /// 1. Sort characters by page, then by Y (descending = top-to-bottom),
    ///    then by X (ascending = left-to-right)
    /// 2. Group into lines: chars with Y within `line_tolerance` of each other
    /// 3. Within a line, insert space when X gap > `space_threshold`
    fn reconstruct_lines(chars: &[PdfChar]) -> Vec<TextLine> {
        if chars.is_empty() {
            return Vec::new();
        }

        let mut sorted = chars.to_vec();
        sorted.sort_by(|a, b| {
            a.page.cmp(&b.page)
                .then(b.y.partial_cmp(&a.y).unwrap_or(std::cmp::Ordering::Equal))
                .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
        });

        let mut lines: Vec<TextLine> = Vec::new();
        let mut current_line_chars: Vec<PdfChar> = vec![sorted[0].clone()];
        let line_tolerance = sorted[0].height * 0.4; // 40% of font height

        for ch in sorted.iter().skip(1) {
            let last = current_line_chars.last().unwrap();

            // Same line? Same page and Y within tolerance
            if ch.page == last.page && (ch.y - last.y).abs() < line_tolerance {
                current_line_chars.push(ch.clone());
            } else {
                // Flush current line
                lines.push(Self::build_line(&current_line_chars));
                current_line_chars = vec![ch.clone()];
            }
        }

        // Flush last line
        if !current_line_chars.is_empty() {
            lines.push(Self::build_line(&current_line_chars));
        }

        lines
    }

    /// Build a TextLine from grouped characters, inserting spaces at gaps
    fn build_line(chars: &[PdfChar]) -> TextLine {
        let mut text = String::new();
        let avg_char_width = chars.iter()
            .map(|c| c.width)
            .sum::<f32>() / chars.len() as f32;
        let space_threshold = avg_char_width * 0.3; // 30% of avg width = gap

        for (i, ch) in chars.iter().enumerate() {
            if i > 0 {
                let gap = ch.x - (chars[i - 1].x + chars[i - 1].width);
                if gap > space_threshold {
                    text.push(' ');
                }
            }
            text.push(ch.ch);
        }

        TextLine {
            text,
            x: chars[0].x,
            y: chars[0].y,
            chars: chars.to_vec(),
            page: chars[0].page,
        }
    }

    /// Render lines to markdown, with table regions converted to markdown tables
    fn render_markdown(lines: &[TextLine], tables: &[Table]) -> String {
        let mut output = String::new();
        let mut in_table: Option<usize> = None; // index into tables vec
        let mut table_rendered: Vec<bool> = vec![false; tables.len()];

        for line in lines {
            // Check if this line belongs to a detected table
            let table_idx = tables.iter().position(|t| {
                line.page == t.page
                    && line.y >= t.y_min
                    && line.y <= t.y_max
                    && line.x >= t.x_min - 5.0
                    && line.x <= t.x_max + 5.0
            });

            if let Some(idx) = table_idx {
                if !table_rendered[idx] {
                    // Render the entire table as markdown table
                    output.push('\n');
                    output.push_str(&tables[idx].to_markdown());
                    output.push('\n');
                    table_rendered[idx] = true;
                }
                // Skip individual table lines (already rendered)
                continue;
            }

            // Regular text line
            let trimmed = line.text.trim();
            if trimmed.is_empty() {
                continue;
            }

            // Heuristic heading detection: large font + short line
            let avg_height = line.chars.iter()
                .map(|c| c.height)
                .sum::<f32>() / line.chars.len() as f32;

            if avg_height > 16.0 && trimmed.len() < 100 {
                output.push_str(&format!("## {trimmed}\n\n"));
            } else if avg_height > 13.0 && trimmed.len() < 120 {
                output.push_str(&format!("### {trimmed}\n\n"));
            } else {
                output.push_str(trimmed);
                output.push('\n');
            }
        }

        output
    }
}

impl ContentHandler for PdfHandler {
    fn supported_types(&self) -> &[&str] {
        &["application/pdf"]
    }

    fn to_markdown(&self, bytes: &[u8], content_type: &str) -> Result<ConversionResult> {
        let start = std::time::Instant::now();

        let (chars, page_count) = Self::extract_chars(bytes)?;
        let lines = Self::reconstruct_lines(&chars);
        let tables = detect_tables(&lines);
        let markdown = Self::render_markdown(&lines, &tables);

        Ok(ConversionResult {
            markdown,
            page_count: Some(page_count),
            content_type: content_type.to_string(),
            elapsed_ms: start.elapsed().as_secs_f64() * 1000.0,
        })
    }
}
```

### 4.2 Table Detection Algorithm

The core insight: tables in PDFs are visually aligned columns. Characters in a table column
share similar X positions across rows, while characters in a table row share similar Y positions.

```rust
// src/content/table.rs

/// A detected table in the PDF
#[derive(Debug, Clone)]
pub struct Table {
    pub page: usize,
    pub x_min: f32,
    pub x_max: f32,
    pub y_min: f32,
    pub y_max: f32,
    pub rows: Vec<Vec<String>>,  // rows[row_idx][col_idx] = cell text
}

impl Table {
    /// Render as markdown table
    pub fn to_markdown(&self) -> String {
        if self.rows.is_empty() {
            return String::new();
        }

        let col_count = self.rows.iter().map(|r| r.len()).max().unwrap_or(0);
        if col_count == 0 {
            return String::new();
        }

        let mut md = String::new();

        // Header row
        let header = &self.rows[0];
        md.push('|');
        for col in 0..col_count {
            md.push_str(&format!(" {} |", header.get(col).map(|s| s.as_str()).unwrap_or("")));
        }
        md.push('\n');

        // Separator
        md.push('|');
        for _ in 0..col_count {
            md.push_str(" --- |");
        }
        md.push('\n');

        // Data rows
        for row in self.rows.iter().skip(1) {
            md.push('|');
            for col in 0..col_count {
                md.push_str(&format!(" {} |", row.get(col).map(|s| s.as_str()).unwrap_or("")));
            }
            md.push('\n');
        }

        md
    }
}

/// Detect tables from reconstructed text lines.
///
/// Algorithm:
///
/// 1. **Column detection**: For each line, find character X positions that
///    could be column boundaries (large gaps > 2x average char width).
///
/// 2. **Column alignment**: Group consecutive lines (same page) that share
///    similar column boundary positions (within tolerance). A run of 3+
///    lines with aligned columns = candidate table region.
///
/// 3. **Cell extraction**: For each row in the table region, split text at
///    the detected column boundaries.
///
/// Complexity: O(L * C) where L = lines, C = max columns per line.
/// For a typical 10-page PDF: ~500 lines * ~10 columns = ~5000 ops, negligible.
pub fn detect_tables(lines: &[super::pdf::TextLine]) -> Vec<Table> {
    let mut tables = Vec::new();

    // Group lines by page
    let mut page_groups: std::collections::BTreeMap<usize, Vec<&super::pdf::TextLine>> =
        std::collections::BTreeMap::new();
    for line in lines {
        page_groups.entry(line.page).or_default().push(line);
    }

    for (page, page_lines) in &page_groups {
        // Step 1: Find column boundaries for each line
        let line_boundaries: Vec<Vec<f32>> = page_lines
            .iter()
            .map(|line| find_column_boundaries(line))
            .collect();

        // Step 2: Find runs of aligned boundaries
        let mut run_start = 0;
        while run_start < page_lines.len() {
            let mut run_end = run_start + 1;

            // Extend run while column boundaries align
            while run_end < page_lines.len() {
                if boundaries_align(&line_boundaries[run_start], &line_boundaries[run_end], 5.0) {
                    run_end += 1;
                } else {
                    break;
                }
            }

            // Need 3+ aligned lines to call it a table
            let run_len = run_end - run_start;
            if run_len >= 3 && !line_boundaries[run_start].is_empty() {
                // Step 3: Extract cells
                let boundaries = &line_boundaries[run_start];
                let rows: Vec<Vec<String>> = page_lines[run_start..run_end]
                    .iter()
                    .map(|line| split_at_boundaries(line, boundaries))
                    .collect();

                let table_lines = &page_lines[run_start..run_end];
                tables.push(Table {
                    page: *page,
                    x_min: table_lines.iter().map(|l| l.x).fold(f32::INFINITY, f32::min),
                    x_max: table_lines.iter().map(|l| {
                        l.chars.last().map(|c| c.x + c.width).unwrap_or(l.x)
                    }).fold(f32::NEG_INFINITY, f32::max),
                    y_min: table_lines.iter().map(|l| l.y).fold(f32::INFINITY, f32::min),
                    y_max: table_lines.iter().map(|l| l.y).fold(f32::NEG_INFINITY, f32::max),
                    rows,
                });
            }

            run_start = run_end;
        }
    }

    tables
}

/// Find X positions where column gaps occur in a text line.
/// A column gap = horizontal space > 2x the average character width in that line.
fn find_column_boundaries(line: &super::pdf::TextLine) -> Vec<f32> {
    if line.chars.len() < 2 {
        return Vec::new();
    }

    let avg_width: f32 = line.chars.iter().map(|c| c.width).sum::<f32>()
        / line.chars.len() as f32;
    let gap_threshold = avg_width * 2.0;

    let mut boundaries = Vec::new();
    for i in 1..line.chars.len() {
        let gap = line.chars[i].x - (line.chars[i - 1].x + line.chars[i - 1].width);
        if gap > gap_threshold {
            // Boundary at the midpoint of the gap
            boundaries.push(line.chars[i - 1].x + line.chars[i - 1].width + gap / 2.0);
        }
    }
    boundaries
}

/// Check if two sets of column boundaries are aligned within tolerance
fn boundaries_align(a: &[f32], b: &[f32], tolerance: f32) -> bool {
    if a.len() != b.len() || a.is_empty() {
        return false;
    }
    a.iter().zip(b.iter()).all(|(ax, bx)| (ax - bx).abs() < tolerance)
}

/// Split a line's text at column boundaries, producing cell strings
fn split_at_boundaries(line: &super::pdf::TextLine, boundaries: &[f32]) -> Vec<String> {
    let mut cells: Vec<String> = vec![String::new(); boundaries.len() + 1];

    for ch in &line.chars {
        let col = boundaries.iter().position(|&b| ch.x < b).unwrap_or(boundaries.len());
        cells[col].push(ch.ch);
    }

    cells.iter().map(|s| s.trim().to_string()).collect()
}
```

**Why this algorithm over ML-based table detection**: ML adds 100MB+ model weight and 100ms+
latency. The column-alignment heuristic handles 90%+ of real-world PDF tables (academic papers,
invoices, reports) at zero additional dependency cost. Edge cases (spanning cells, nested tables)
are rare in LLM-consumption scenarios.

## 5. Feature Flag Design in Cargo.toml

```toml
[features]
default = ["cli", "http3"]
cli = ["clap"]
http3 = ["quinn", "h3", "h3-quinn"]
pdf = ["pdfium-render"]                    # NEW: opt-in PDF support

[dependencies]
# ... existing deps ...

# PDF extraction (optional - adds ~4MB to binary)
pdfium-render = { version = "0.8", optional = true }
```

**Usage**:
```bash
# Default build (no PDF, same binary size as today)
cargo build --release

# With PDF support
cargo build --release --features pdf

# Full build
cargo build --release --features "cli,http3,pdf"
```

**CI matrix**: Test both `--features pdf` and without. The `#[cfg(feature = "pdf")]` gates
in `content/mod.rs` ensure clean compilation either way.

## 6. Content-Type Routing in main.rs

### Integration Point

The change is minimal. In `cmd_fetch` (`src/main.rs:~676-742`), where the response body
is currently consumed as text and passed to `html_to_markdown`, we instead:

1. Read the `Content-Type` header from the response
2. Get the body as **bytes** (not text -- PDF is binary)
3. Route through `ContentRouter`

```rust
// In cmd_fetch, replace the body handling in the Full/Compact/Json arms:

// BEFORE:
//   let body_text = response.text().await?;
//   ...
//   output_body(&body_text, output_file, markdown, links, max_body)?;

// AFTER:
let content_type = response
    .headers()
    .get("content-type")
    .and_then(|v| v.to_str().ok())
    .unwrap_or("text/html")
    .to_string();

let body_bytes = response.bytes().await?;

let output_text = if markdown {
    // Route through content handler
    let router = nab::content::ContentRouter::new();
    let result = tokio::task::spawn_blocking(move || {
        router.convert(&body_bytes, &content_type)
    }).await??;

    if matches!(format, OutputFormat::Full) {
        if let Some(pages) = result.page_count {
            println!("   Pages: {pages}");
            println!("   Conversion: {:.1}ms", result.elapsed_ms);
        }
    }
    result.markdown
} else {
    // Raw output (--raw-html flag)
    String::from_utf8_lossy(&body_bytes).to_string()
};
```

### MCP Server Integration

Same pattern in `src/bin/mcp_server.rs` FetchTool::run():

```rust
// After getting the response, before outputting body:
let content_type = response
    .headers()
    .get("content-type")
    .and_then(|v| v.to_str().ok())
    .unwrap_or("text/html")
    .to_string();

let body_bytes = response.bytes().await
    .map_err(|e| CallToolError::from_message(e.to_string()))?;

let router = nab::content::ContentRouter::new();
let result = tokio::task::spawn_blocking(move || {
    router.convert(&body_bytes, &content_type)
}).await
    .map_err(|e| CallToolError::from_message(e.to_string()))?
    .map_err(|e| CallToolError::from_message(e.to_string()))?;

// Use result.markdown as the body text
```

## 7. Future Extensibility

### 7.1 `nab submit` (Form POST)

The `ContentHandler` trait handles **response** conversion. Form submission is an **input**
concern. The design uses a separate `FormEncoder` concept that produces request bodies:

```rust
// Future: src/form/mod.rs (NOT part of this PR)

/// Encodes structured data into HTTP request bodies
pub trait FormEncoder: Send + Sync {
    /// Content-Type header to set on the request
    fn content_type(&self) -> &str;

    /// Encode fields into request body bytes
    fn encode(&self, fields: &[(&str, &str)]) -> Result<Vec<u8>>;
}

// Implementations:
// - UrlEncodedFormEncoder  (application/x-www-form-urlencoded)
// - MultipartFormEncoder   (multipart/form-data, for file uploads)
// - JsonFormEncoder        (application/json)
```

**CLI surface** (future):
```bash
nab submit https://example.com/api \
    --field name=value \
    --field file=@path/to/file \
    --encoding multipart
```

The response from `submit` flows through the same `ContentRouter`, so PDF/HTML/JSON responses
from form submissions are automatically handled.

### 7.2 `nab login` (Auth Flow)

Login combines existing auth primitives (`CookieSource`, `OnePasswordAuth`, `OtpRetriever`)
into a multi-step flow:

```
nab login https://example.com
  1. Discover login form (fetch page, find <form> with password input)
  2. Look up credentials (1Password)
  3. Submit form (FormEncoder)
  4. Handle MFA if needed (OtpRetriever)
  5. Capture session cookies
  6. Store cookies for future requests
```

The `ContentHandler` trait is relevant here because step 1 (discover login form) needs to
parse HTML. The existing `HtmlHandler` can be reused, but `nab login` will also need a
dedicated `FormDiscovery` module that extracts `<form>` structure (action URL, field names,
hidden CSRF tokens).

### 7.3 Additional Content Handlers (Future)

| Handler | MIME Type | Dependency | Priority |
|---------|-----------|------------|----------|
| `DocxHandler` | application/vnd.openxmlformats... | `quick-xml` | Low |
| `CsvHandler` | text/csv | none (stdlib) | Medium |
| `ImageHandler` | image/* | Vision API or alt-text extraction | Low |
| `XlsxHandler` | application/vnd.openxmlformats... | `calamine` | Medium |

Each is added by:
1. Create `src/content/{name}.rs` implementing `ContentHandler`
2. Add to `ContentRouter::new()` handler list
3. Optionally gate behind a feature flag

No changes to the trait, router, or existing handlers.

## Performance Budget

| Operation | Target | Measured (estimate) |
|-----------|--------|---------------------|
| Content-Type routing | <0.1ms | HashMap lookup, negligible |
| HTML -> Markdown | ~5ms/page | Existing html2md performance |
| PDF char extraction | ~5ms/page | pdfium FFI is fast |
| Line reconstruction | ~1ms/page | In-memory sort + scan |
| Table detection | ~1ms/page | O(lines * cols) |
| PDF total pipeline | ~10ms/page | Sum of above with overhead |

**Benchmark plan**: Add criterion bench in `benches/pdf_benchmark.rs` using a 10-page
reference PDF. Gate behind `#[cfg(feature = "pdf")]`.

## Migration Plan

### Phase 1: Content Handler Framework (this PR)
1. Create `src/content/mod.rs` with trait + router
2. Move `html_to_markdown` to `src/content/html.rs`
3. Create `src/content/plain.rs` (trivial passthrough)
4. Wire `ContentRouter` into `cmd_fetch` and MCP server
5. Tests: existing HTML behavior preserved, plain text passthrough

### Phase 2: PDF Handler
1. Add `pdfium-render` optional dependency
2. Implement `PdfHandler` + `table.rs`
3. Tests: reference PDFs (text-only, single table, multi-table, multi-page)
4. Benchmark: criterion suite

### Phase 3: Polish
1. Add page range selection (`--pages 1-5`)
2. Add `--raw-pdf` flag to skip conversion
3. Update README and `--help` text

## Risks & Mitigations

| Risk | Impact | Mitigation |
|------|--------|------------|
| pdfium static linking fails on some platforms | Build breaks | Use `pdfium-render`'s dynamic binding fallback; document setup |
| Table detection misidentifies paragraphs as tables | Bad markdown output | Require 3+ aligned rows (strict); font-size heuristic to exclude body text |
| pdfium not thread-safe | Panic in `spawn_blocking` | pdfium-render handles thread safety internally; one doc per task |
| Binary size regression | User complaints | Feature flag (opt-in); document in README |
| Scanned PDF (images, no text layer) | Empty output | Detect empty text extraction, output warning: "Scanned PDF - no text layer detected" |

## Testing Strategy

```
tests/
├── content/
│   ├── test_html_handler.rs      # Existing html_to_markdown behavior
│   ├── test_pdf_handler.rs       # Unit tests with embedded PDF bytes
│   ├── test_plain_handler.rs     # Passthrough verification
│   ├── test_table_detection.rs   # Column boundary + alignment logic
│   └── test_router.rs            # Content-Type dispatch
└── fixtures/
    ├── simple.pdf                # Text-only PDF
    ├── table.pdf                 # PDF with tables
    └── multi_page.pdf            # Multi-page document
```

**Key test cases**:
- HTML Content-Type routes to HtmlHandler (regression)
- `application/pdf` routes to PdfHandler (new)
- `text/plain` and `application/json` pass through unchanged
- Unknown Content-Type with HTML-like content falls back to HTML
- PDF with tables produces valid markdown table syntax
- Empty/scanned PDF produces helpful error message
- Feature flag disabled: PDF Content-Type falls through to plain text

---

*Architecture designed for nab v0.3.x. Reviewed against existing patterns in
`src/stream/backend.rs` (trait-based dispatch), `src/content/` (new module),
and `Cargo.toml` feature flags (existing `http3` pattern).*
