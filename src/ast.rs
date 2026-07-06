//! Parser-isolation seam: the one module in pgsafe that names the SQL parser
//! crate. Every rule and analysis imports its AST types from here (as
//! `crate::ast::…`), so switching the parser backend — a source-compatible
//! fork, a `pg_query` version bump, or an API-compatible successor — is a
//! change to this file (and `Cargo.toml`) alone, not to the dozens of call
//! sites across the rule tree. (A parser with a differently shaped AST would
//! still ripple into the rules, which match `pg_query`'s concrete node types
//! directly; the seam isolates the dependency, not the AST shape.)
//!
//! Fidelity requirement: the backend MUST be the real PostgreSQL grammar via
//! libpg_query. An approximate parser would produce false negatives, and a
//! false negative in a migration-safety linter ships a dangerous migration.

// `pub(crate)`, not `pub`: the re-exports must stay crate-internal even if this
// module is later made `pub` — the seam exists so a `pg_query` type never leaks
// into pgsafe's public API.
pub(crate) use pg_query::protobuf;
pub(crate) use pg_query::{parse, parse_plpgsql, scan, NodeEnum};
