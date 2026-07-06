//! Parser-isolation seam: the one module in pgsafe that names the SQL parser
//! crate. Every rule and analysis imports its AST types from here (as
//! `crate::ast::…`), so swapping the parser backend — a fork, a future upstream
//! bump, or a different backend entirely — is a change to this file (and
//! `Cargo.toml`) alone, not to the dozens of call sites across the rule tree.
//!
//! Fidelity requirement: the backend MUST be the real PostgreSQL grammar via
//! libpg_query. An approximate parser would produce false negatives, and a
//! false negative in a migration-safety linter ships a dangerous migration.

pub use pg_query::protobuf;
pub use pg_query::{parse, parse_plpgsql, scan, NodeEnum};
