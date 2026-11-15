//! How the mapping stands: where the unmapped money is (`gaps`), rule
//! suggestions for it (`suggest`), how much is mapped per month (`coverage`),
//! and what's new to map, as a short message (`digest`). Works on a build from
//! the engine; does no IO.

pub mod coverage;
pub mod digest;
pub mod gaps;
pub mod suggest;
