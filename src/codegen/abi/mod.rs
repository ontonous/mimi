//! Native ABI facts shared by the legacy and resolved emitters.
//!
//! This module is deliberately the only place where value ownership classes
//! and ABI slot-widening policy are defined.  Emitters may adapt surface or
//! checker-owned types into these facts, but must not grow parallel policy
//! tables at individual expression sites.

pub(in crate::codegen) mod layout;
pub(in crate::codegen) mod ownership;
