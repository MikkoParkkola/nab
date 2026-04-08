//! Build script — no-op.
//!
//! Previously wired in Swift runtime link paths for the `fluidaudio-rs`
//! in-process FFI crate. That crate has been removed; `FluidAudio` is now
//! invoked as a subprocess (`fluidaudiocli`) with no Swift link requirements.

fn main() {}
