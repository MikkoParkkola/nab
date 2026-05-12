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
//! See [`ingestion_guard`] for the detector + sanitiser. `WebMCP`
//! advertisement detection is informational by default and can be made
//! strict with `NAB_WEBMCP_STRICT=true` plus an explicit
//! `NAB_WEBMCP_OPT_IN` allow-list.

pub mod fetch_yara;
pub mod ingestion_guard;
pub mod yara_engine;

pub use fetch_yara::{guard_fetch_output, guard_fetch_output_with_config};
pub use ingestion_guard::{
    DetectionReport, DirectiveKind, IngestionGuardError, IngestionPolicy, Sample, Severity, detect,
    enforce_policy, sanitize, sanitize_with_env_policy, sanitize_with_policy,
};
