//! `hungjury` — typed decisions from a jury of headless CLI agents.
//!
//! The binary is a thin CLI over this library: `decide` orchestrates
//! juror calls + voting + escalation; `memory` keeps judge knowledge in a
//! local SQLite store; `learn`/`eval`/`feedback` drive offline learning.

pub mod backend;
pub mod cache;
pub mod config;
pub mod doctor;
pub mod error;
pub mod eval;
pub mod judge;
pub mod jury;
pub mod learn;
pub mod memory;
pub mod prompt;
pub mod question;
pub mod quota;
pub mod request;
pub mod response;
pub mod sys;
pub mod util;
