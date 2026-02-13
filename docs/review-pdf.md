# Code Review: Content Handler Architecture & PDF Support

**Reviewer**: reviewer (automated)
**Date**: 2026-02-13
**Scope**: Architecture design (`docs/architecture-pdf.md`), content handler module (`src/content/`), Cargo.toml changes, lib.rs integration

---

## Summary

The architecture design is well-reasoned and the content handler framework implementation is clean, idiomatic Rust. The trait design, module layout, and test coverage for the non-PDF handlers are solid. However, there is one **blocking issue** (pdfium-render compilation failure) and several findings that should be addressed before merge.

**Verdict**: Ship with follow-up fixes for blocking issue.

---

## Blocking Issues

### B1. `pdfium-render` 0.8.37 fails to compile on Rust 1.93

**Evidence**: `cargo check --features pdf` produces 122 errors in `pdfium-render` itself (missing methods like `FPDFBitmap_FillRect`, type annotation failures, removed APIs). This is a compatibility issue between `pdfium-render` 0.8.x and the current Rust toolchain.

**Impact**: The `pdf` feature cannot be built, which means no PDF support ships.

**Fix options** (choose one):
1. Upgrade to `pdfium-render` 0.9.x (if available) which may support newer Rust.
2. Pin an older Rust version for PDF builds (undesirable, conflicts with MSRV 1.93).
3. Switch to an alternative PDF extraction library (e.g., `pdf-extract`, `lopdf` + custom text extraction).
4. Downgrade to a known-working `pdfium-render` version with `=0.8.X` pinning.

**Priority**: P0 -- without this fix, the PDF feature is dead code.

---

## Architecture Review

### Strengths

**A1. Clean trait design** (`/Users/mikko/github/nab/src/content/mod.rs:53-62`)
- `ContentHandler` is stateless + sync, matching the correct design for pdfium FFI.
- `Send + Sync` bounds align with existing project patterns (`StreamProvider`, `StreamBackend`).
- `&[u8]` input is the right choice for binary formats like PDF.
- The trait is minimal (2 methods) -- follows Rams #10 ("as little design as possible").

**A2. Correct feature gating** (`/Users/mikko/github/nab/src/content/mod.rs:27-31`)
- `#[cfg(feature = "pdf")]` consistently gates both `pub mod pdf` and `pub mod table`.
- Clean compilation with and without the `pdf` feature (verified: `cargo check` passes without feature).

**A3. Fallback chain is well-designed** (`/Users/mikko/github/nab/src/content/mod.rs:94-121`)
- MIME type extraction strips charset parameters correctly.
- HTML-like byte sniffing catches the common case of missing Content-Type headers.
- Ultimate fallback to `PlainHandler` prevents panics on unknown types.

**A4. Test coverage is adequate for non-PDF handlers**
- 16 tests pass covering: HTML conversion, boilerplate filtering, link preservation, charset handling, JSON passthrough, empty input, non-UTF8, router dispatch for all content types, fallback behavior.

**A5. Architecture document is thorough**
- Clear problem statement, design constraints, module layout.
- Future extensibility paths (`nab submit`, `nab login`, additional handlers) are well-scoped.
- Risks and mitigations are explicitly listed.
- Migration plan is phased (framework first, PDF second, polish third).

### Concerns

**A6. ContentRouter is not yet wired into main.rs**

The architecture doc (Section 6) specifies replacing `response.text().await?` with `response.bytes().await?` and routing through `ContentRouter`. The current `main.rs` diff only adds SPA auto-extraction -- it still calls `html_to_markdown()` directly at lines 758 and 783 (original), not `ContentRouter::convert()`. The content module exists but is not used by the CLI.

**Impact**: Medium. The content handler framework is testable in isolation, but PDF URLs will still be mangled through `html_to_markdown` at runtime.

**Fix**: Wire `ContentRouter` into `cmd_fetch` as specified in the architecture doc Section 6.

**A7. `html_to_markdown` is duplicated**

The original `html_to_markdown` function remains in `main.rs:804-817` and an identical copy exists in `/Users/mikko/github/nab/src/content/html.rs:37-48`. The architecture doc correctly calls for removing the original from `main.rs`, but this has not been done.

**Impact**: Low (functional duplication, not a bug). Should be cleaned up.

---

## PDF Handler Review (`/Users/mikko/github/nab/src/content/pdf.rs`)

### Strengths

**P1. Scanned PDF detection** (lines 216-226)
- Empty chars + non-zero page count correctly produces a helpful message instead of empty output.

**P2. Heading heuristics** (lines 191-198)
- Font height thresholds (>16pt for `##`, >13pt for `###`) with line length caps are reasonable defaults for most document layouts.

**P3. Line reconstruction algorithm** (lines 87-126)
- Sort order (page, Y descending, X ascending) is correct for top-to-bottom, left-to-right reading.
- Line tolerance at 40% of font height is reasonable for mixed-font documents.
- Space insertion at 30% of average char width handles proportional fonts adequately.

### Concerns

**P4. Potential panic on NaN float comparison** (`/Users/mikko/github/nab/src/content/pdf.rs:97-103`)

The sort uses `partial_cmp` with `unwrap_or(Ordering::Equal)` for float comparison. While this handles NaN gracefully (treats NaN as equal to everything), if a PDF produces NaN coordinates, the sort result will be non-deterministic. pdfium should never produce NaN, but defensive code would be safer.

**Impact**: Low (theoretical). pdfium returns finite float values for valid PDFs.

**P5. `chars.to_vec()` cloning in multiple places**

- `reconstruct_lines` clones the entire char vector at line 92 (`chars.to_vec()`)
- `build_line` clones again at line 149 (`chars.to_vec()`)

For large PDFs (1000+ pages), this means O(n) allocations of the full character set. The `TextLine` struct stores owned `Vec<PdfChar>` including the full chars from `build_line`.

**Impact**: Medium for very large PDFs. For the target use case (<100 pages), this is fine. Consider taking ownership or using references in a future optimization pass if profiling shows allocation pressure.

**P6. No size limit on input bytes**

`extract_chars` passes the full byte slice to pdfium without checking size. A malicious or accidentally huge PDF (100MB+) could cause excessive memory usage.

**Impact**: Medium. The architecture doc mentions this in the risks section for "Binary size regression" but not for memory safety. Consider adding a configurable max size (e.g., 50MB default) with a clear error message.

**P7. Encrypted PDF handling**

`load_pdf_from_byte_slice(bytes, None)` passes `None` for the password. Encrypted PDFs will fail with a pdfium error. The architecture doc lists this as a risk but the implementation does not provide a user-friendly error message.

**Impact**: Low. The `anyhow` context ("Failed to parse PDF") will surface, but a more specific message like "PDF is password-protected" would be better UX.

---

## Table Detection Review (`/Users/mikko/github/nab/src/content/table.rs`)

### Strengths

**T1. Algorithm is sound**
- Column boundary detection via gap analysis (2x average char width) is a proven heuristic.
- Boundary alignment tolerance (5.0 points) handles minor typographic variance.
- Minimum 3-row requirement prevents false positives from paragraph text.

**T2. Good test coverage**
- 8 tests cover: empty table rendering, simple tables, ragged rows, aligned column detection, plain text rejection, boundary alignment (exact, within tolerance, different count, out of tolerance).

### Concerns

**T3. Boundary alignment uses first row as reference** (`/Users/mikko/github/nab/src/content/table.rs:100-113`)

The algorithm compares all subsequent rows against `line_boundaries[run_start]`. If the first row of a table is a header with different spacing than data rows, the table may be split. More robust: compare each row against its predecessor (`run_end - 1`).

**Impact**: Low for most real-world tables. Headers typically have the same column positions as data rows.

**T4. `find_column_boundaries` can produce false boundaries for wide characters**

If a line contains a mix of narrow (e.g., `l`, `i`) and wide (e.g., `W`, `M`) characters, the average width is skewed, potentially producing false column boundaries within normal text.

**Impact**: Low. The 2x multiplier provides sufficient margin for proportional fonts.

**T5. Circular module dependency**

`table.rs` imports `super::pdf::TextLine` and `super::pdf::PdfChar` (line 13 and test line 211). This creates a tight coupling between `table.rs` and `pdf.rs`. If a future handler (e.g., DOCX) also needs table detection, it would need to use the same `TextLine`/`PdfChar` types.

**Impact**: Low now, medium for future extensibility. Consider moving `TextLine` and `PdfChar` into `mod.rs` or a shared `types.rs` module.

---

## Style & Idioms

**S1. Unused `mut` warning** (`/Users/mikko/github/nab/src/content/mod.rs:78`)

```
warning: variable does not need to be mutable
  --> src/content/mod.rs:78:13
   |
78 |         let mut handlers: Vec<Box<dyn ContentHandler>> = vec![
```

When compiled without the `pdf` feature, the `handlers` vec is never mutated (no `insert`). The `mut` is only needed with `#[cfg(feature = "pdf")]`.

**Fix**: Gate the `mut` with `#[cfg_attr(feature = "pdf", allow(unused_mut))]` or restructure:

```rust
let handlers: Vec<Box<dyn ContentHandler>> = {
    let mut h: Vec<Box<dyn ContentHandler>> = vec![...];
    #[cfg(feature = "pdf")]
    h.insert(0, Box::new(pdf::PdfHandler::new()));
    h
};
```

**S2. Documentation quality is high**
- Module-level `//!` docs on all files.
- Doc comments on all public items.
- Algorithm descriptions in comments.
- Example in `mod.rs` module docs.

**S3. Code follows existing project patterns**
- Uses `anyhow::Result` consistently.
- Test modules are in-file with `#[cfg(test)]`.
- Feature flags follow the existing `http3` pattern.

---

## License Compatibility

| Component | License | Compatible with nab (MIT)? |
|-----------|---------|---------------------------|
| `pdfium-render` crate | MIT OR Apache-2.0 | Yes |
| PDFium (Chromium) | BSD-3-Clause | Yes |
| nab | MIT | N/A (source) |

All licenses are permissive and compatible. No issues.

---

## Test Results

| Suite | Result | Count |
|-------|--------|-------|
| `cargo check` (no pdf) | PASS (1 warning) | -- |
| `cargo check --features pdf` | **FAIL** (122 errors in pdfium-render) | -- |
| `cargo test --lib content` (no pdf) | PASS | 16/16 |

---

## Recommendations Summary

| ID | Severity | Category | Issue | Action |
|----|----------|----------|-------|--------|
| **B1** | BLOCKING | Build | pdfium-render 0.8.37 fails to compile on Rust 1.93 | Fix dependency version or switch library |
| A6 | High | Integration | ContentRouter not wired into main.rs cmd_fetch | Complete Section 6 of architecture doc |
| A7 | Low | Hygiene | Duplicate html_to_markdown function | Remove original from main.rs |
| P5 | Medium | Performance | Excessive cloning of char vectors for large PDFs | Optimize in follow-up if profiled |
| P6 | Medium | Safety | No input size limit on PDF bytes | Add configurable max size |
| P7 | Low | UX | No specific error for encrypted PDFs | Improve error message |
| S1 | Low | Style | Unused mut warning without pdf feature | Restructure handlers initialization |
| T3 | Low | Correctness | Table boundary comparison only against first row | Compare against predecessor |
| T5 | Low | Extensibility | TextLine/PdfChar types tightly coupled to pdf.rs | Move to shared types module |

---

## Conclusion

The content handler architecture is well-designed and follows the project's existing patterns. The `ContentHandler` trait, `ContentRouter`, `HtmlHandler`, and `PlainHandler` are production-ready. The PDF handler and table detection algorithms are algorithmically sound with good test coverage for the unit logic.

The **blocking issue** is the `pdfium-render` compilation failure on Rust 1.93. This must be resolved before the `pdf` feature can be used. Additionally, the `ContentRouter` needs to be wired into `main.rs` to complete the integration.

With B1 and A6 resolved, this is ready to ship.
