# Contributing to pgsafe

## Build & test

```sh
cargo test                                          # run the full test suite
cargo clippy --all-targets -- -D warnings           # must be warning-free (compiles benches too)
cargo fmt                                           # format check
```

The library core builds without `clap` (the CLI lives behind the default `cli` feature):

```sh
cargo build --no-default-features   # compiles the embeddable core, no binary
```

## Benchmarks

```sh
cargo bench                                         # run criterion suite; HTML report in target/criterion/
cargo bench --no-run                                # compile-only (useful in CI to catch bench bit-rot)
```

Benchmarks live in `benches/lint.rs` and cover three input sizes:

| Benchmark | Input | ~Time (dev box) |
|-----------|-------|-----------------|
| `lint_small` | single `CREATE INDEX` | ~3.3 µs |
| `lint_medium_50` | 50 FK statements | ~303 µs |
| `lint_large_1000` | 1 000 `SET NOT NULL` statements | ~15 ms |

The hot path is the C parser inside `pg_query`; the rule-walking loop is the part
we own and can optimise.

## Profiling

### Flamegraph (Linux perf)

```sh
cargo install flamegraph
# Profile the bench binary directly:
cargo flamegraph --bench lint -- --bench
# Or build the release CLI and profile a big migration file:
cargo build --release
flamegraph -- ./target/release/pgsafe big_migration.sql
```

The SVG is written to `flamegraph.svg` in the current directory.

### samply (cross-platform, no root required)

```sh
cargo install samply
cargo build --release
samply record ./target/release/pgsafe big_migration.sql
```

samply opens a Firefox Profiler tab in your browser with the captured profile.

## Rule ids are public API

A rule's `id()` is the contract that inline `-- pgsafe:ignore <rule-id>` directives
target. Renaming an id silently breaks every migration that suppresses it. Treat
rule ids as stable: add new rules with new ids; do not rename existing ones.

## Proving rules against real Postgres

Each rule claims a lock, rewrite, outright-failure, blocking, or plan-invalidation hazard.
`tests/rule_proofs.rs` proves those claims empirically against a real Postgres: it reads the held
lock from `pg_locks`, detects a table rewrite via a `relfilenode` change, confirms statements that
must fail do (by SQLSTATE), shows a blocking statement blocks a concurrent reader, and shows that a
non-rewriting `ALTER COLUMN … TYPE` still breaks a cached plan (`cached plan must not change result
type`). These tests are `#[ignore]`d (they need a database), so the normal `cargo test` run and the
PR gate stay DB-free.

Run them against one database:

```sh
DATABASE_URL=postgres://postgres:secret@127.0.0.1:55459/postgres \
  cargo test --test rule_proofs -- --ignored --nocapture
```

Or across the whole supported version matrix (spins up throwaway Docker Postgres 14–18):

```sh
scripts/prove-rules.sh            # all versions
scripts/prove-rules.sh 16         # just one
```

The same proofs run in CI via the `rule-proofs` workflow (manual dispatch + weekly), so a
Postgres release that changes a rule's behavior turns that workflow red.
