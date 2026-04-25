// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Secure Ingestion: defensive layer for content fetched from the open web
//! before it is passed to an LLM agent.
//!
//! ## Threat model
//!
//! Public web pages can carry instructions, semantic data, and intercom
//! addresses aimed at AI agents in layers that humans never read: HTML
//! comments addressed to "machine intelligence", machine-only attribute
//! payloads (`data-dim`, `data-ai-*`, …), `class="m"`-style "machine"
//! spans, `display:none` text, and `aria-hidden="true"` content. Whether
//! the publisher is honest (semantic web research) or hostile (supply-chain
//! attack), the agent reading the page cannot tell the difference from the
//! markup alone.
//!
//! Defensive treatment is the same regardless of intent: detect the
//! channel, surface its provenance, and strip it by default before the
//! agent reads the page. Operators can opt back in when they want the
//! machine-readable layer.
//!
//! See [`ingestion_guard`] for the v0 detector + sanitiser. Future work:
//! WebMCP advertisement detection (manifest discovery is in
//! [`crate::webmcp`]) is on the same trust path; promotion of the WebMCP
//! discovery output into a sanctioned/strict policy gate will land here.

pub mod ingestion_guard;

pub use ingestion_guard::{DetectionReport, DirectiveKind, Sample, Severity, detect, sanitize};
