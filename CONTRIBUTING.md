# Contributing to pgsafe

## Build & test

```sh
cargo test                                          # run all 46 tests
cargo clippy --all-targets -- -D warnings           # must be warning-free (compiles benches too)
cargo fmt                                           # format check
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
