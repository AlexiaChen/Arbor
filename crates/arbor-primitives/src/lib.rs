//! Consensus-facing primitive types.
//!
//! Concrete protocol types are introduced in M2. Keeping this crate free of
//! runtime, storage, and networking dependencies is an architectural boundary.

#![forbid(unsafe_code)]

/// Version of the Arbor wire and canonical encoding rules.
pub const PROTOCOL_VERSION: u32 = 1;
