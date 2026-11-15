//! The engine: turns bank data, rules and config into hledger books. It does no
//! IO: everything comes in as text or values and goes out the same way, so
//! reading and writing the repository is up to `konter-repo`. See docs/spec.md.

pub mod books;
pub mod config;
pub mod dates;
pub mod engine;
pub mod findings;
pub mod hash;
pub mod journal;
pub mod manual;
pub mod model;
pub mod money;
pub mod pipeline;
pub mod rules;
pub mod sources;
pub mod subscriptions;
pub mod transfers;
