/// Nab core library — HTTP fetch primitives for LLM consumption.
///
/// Extracted from the nab binary crate for use as a library dependency
/// in botnaut-client and other portfolio repos.
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
