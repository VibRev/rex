//! What `idax` and `bnx` share: the command line, sessions in worker
//! processes, `save`, `exec` and `ref`, analysis
//! completeness. A tool describes itself in a [`tool::Tool`], implements
//! [`worker::Backend`] for its engine and calls [`tool::main`].
//!
//! The contract is `docs/CONVENTIONS.md`; section numbers in the docs here
//! refer to it. It builds on axkit's; rex depends on no engine SDK and holds
//! no script runtime.

pub mod analysis;
pub mod cli;
pub mod example;
pub mod exec;
pub mod mcp;
pub mod proto;
pub mod pyapi;
pub mod reference;
pub mod save;
pub mod session;
pub mod skills;
pub mod tool;
pub mod worker;
