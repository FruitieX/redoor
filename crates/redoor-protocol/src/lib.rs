//! Wire protocol and REST DTOs shared by every Redoor crate.
//!
//! Kept free of runtime dependencies so DTO changes only rebuild what consumes them and the
//! generated TypeScript bindings have a single source of truth.

pub mod api;
pub mod identity;
pub mod ids;
pub mod wire;
