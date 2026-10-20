//! Host facts that differ between Linux, macOS and Android (Termux).
//!
//! Platform `cfg` branches live here, behind small portable functions, so command and trash
//! logic stays identical on every target and Termux constraints are documented in one place.

pub(crate) mod accounts;
pub(crate) mod identity;
pub(crate) mod mounts;
