//! Bot-trap and adversarial content detection.
//!
//! This module hosts heuristic detectors that run on raw HTML *before*
//! it is converted to markdown. The goal is to recognise pages that
//! exist solely to fingerprint or poison scrapers (e.g. Cloudflare's
//! "AI Labyrinth") and bail out before nab leaks identifying behaviour
//! by following the trap's hidden links.
//!
//! Detectors are intentionally cheap (no embeddings, no network calls)
//! so they can run on every fetch when enabled.
//!
//! See [`labyrinth`] for the AI Labyrinth scorer.

pub mod challenge_scanner;
pub mod labyrinth;

pub use challenge_scanner::{
    ChallengeReference, ChallengeVendor, first_vendor as first_challenge_vendor,
    scan_for_challenges,
};
pub use labyrinth::{LabyrinthScore, Signal, Verdict, detect_labyrinth};
