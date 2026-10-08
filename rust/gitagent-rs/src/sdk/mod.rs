//! `sdk` — the gitagent SDK: agent loading + the four builtin tools + `query()`,
//! built on top of the `pi` engine.

pub mod declarative;
pub mod env;
pub mod eval;
pub mod extract;
pub mod goal;
pub mod hooks;
pub mod learning;
pub mod loader;
pub mod manifest;
pub mod mcp;
pub mod pdf;
pub mod permissions;
pub(crate) mod proc;
pub mod query;
pub mod session;
pub mod telemetry;
pub mod tools;
