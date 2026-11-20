//! How the mapping stands: where the unmapped money is (`gaps`), rule
//! suggestions for it (`suggest`), how much is mapped per month (`coverage`),
//! and what's new to map, as a short message (`digest`). Also what in the
//! store of transactions is left to settle (`unsettled`). Works on a build
//! and a table from the engine; does no IO.

pub mod coverage;
pub mod digest;
pub mod gaps;
pub mod suggest;
pub mod unsettled;

/// Text for HTML, with `&`, `<`, `>` and `"` escaped.
fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
