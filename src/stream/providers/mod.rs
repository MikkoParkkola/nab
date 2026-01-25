//! Streaming service providers

pub mod dr;
pub mod generic;
pub mod nrk;
pub mod svt;
pub mod yle;

pub use dr::DrProvider;
pub use generic::GenericHlsProvider;
pub use nrk::NrkProvider;
pub use svt::SvtProvider;
pub use yle::YleProvider;
