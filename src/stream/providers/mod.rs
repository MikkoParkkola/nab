//! Streaming service providers

pub mod generic;
pub mod yle;

pub use generic::GenericHlsProvider;
pub use yle::YleProvider;
