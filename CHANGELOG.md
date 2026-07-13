# Changelog

All notable changes to pgsafe are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- New rule `drop-database` (Warning): flags `DROP DATABASE` as irreversible data loss (#95).
- New rule `drop-not-null` (Warning): flags `ALTER COLUMN … DROP NOT NULL` removing a relied-on not-null invariant (#95).
- New rule `add-domain-constraint-without-not-valid` (Error): flags `ALTER DOMAIN … ADD CONSTRAINT` that validates dependent tables under lock; autofixes to `NOT VALID` (#95).
- New opt-in rule `require-schema-qualified` (Warning): flags DDL target relations named without a schema qualifier — CREATE/ALTER/RENAME/DROP/TRUNCATE/CREATE INDEX targets — which resolve through `search_path` (#95).

### Changed

- `--fix` / `--diff` now apply fixes iteratively to a fixpoint (bounded at 10
  passes), so a fix that unblocks another is fully resolved in one run. No change
  for current rules (all converge in a single pass); the loop never writes SQL
  that fails to parse or introduces a new error.
- Human output indent-aligns the continuation lines of multi-line statements so wrapped SQL stays readable (#92).

### Fixed

- `--fix` no longer adds `CONCURRENTLY` to index operations inside an explicit transaction block, where it would be invalid (#91).

## [0.10.0] - 2026-07-06

### Added

- Colorized human output — severity-colored findings, bold rule IDs, dimmed fix suggestions, and an end-of-run summary line (#89).
- `--color <auto|always|never>` controls colorized output; `auto` respects whether stdout is a TTY and honors `NO_COLOR`. With color off, output is byte-for-byte unchanged (#90).

### Changed

- `vacuum-full-cluster` guidance now recommends the built-in `REPACK (CONCURRENTLY)` where available (#87).
- Parser access is isolated behind a `pgsafe::ast` seam, decoupling the linter from the underlying parser (#88).
- Reusable color palette and painters (`Styling`, `ColorWhen`) are exposed for library consumers (#90).

## [0.9.1] - 2026-07-05

### Added

- `pgsafe::VERSION` constant exposing the crate version to library consumers (#86).

### Documentation

- New Usage page; site reorganized to separate reference from how-to guides (#85).

## [0.9.0] - 2026-07-05

### Added

- Published to crates.io — install with `cargo install pgsafe` (#68, #70).
- `--fix` / `--diff` — apply autofixes in place or preview them as a patch (#72, #73, #80).
- `--format sarif` — SARIF 2.1.0 output for GitHub code scanning, with stable `partialFingerprints` for finding de-duplication (#81, #82).

### Changed

- `--diff` output is now `git apply`-able, and in-place fixes are written atomically (#83).

### Fixed

- `require-timeout` places the synthesized `SET` prologue correctly above leading directive comments, own-line directives on non-first statements, and multi-line block-comment directives (#75, #77, #78).

### Documentation

- Playground gained Fix/Ignore buttons and example seeding; Ignore and rule links are gated to real lint rules (#74, #76).

## [0.8.6] - 2026-06-30

### Added

- `pgsafe --list-rules` prints the authoritative rule catalog in human or JSON form.

### Changed

- `attach-partition` escalates ATTACH of a pre-existing child table to an error (#65, #66).

### Documentation

- Launched the pgsafe website (Astro): getting-started, CI, config, and output guides, plus a deep-linkable per-rule reference.
- Added an interactive **playground** that lints SQL in the browser via WebAssembly, with an examples gallery and shareable permalinks.

## [0.8.5] - 2026-06-29

### Changed

- Pin third-party and GitHub-owned Actions to commit SHAs; pin build provenance to the release workflow.

## [0.8.4] - 2026-06-29

### Added

- The GitHub Action verifies build provenance when it downloads pgsafe binaries.

## [0.8.3] - 2026-06-29

### Added

- Release binaries carry signed build-provenance attestations; releases publish via draft → upload → publish.

## [0.8.2] - 2026-06-29

### Changed

- The GitHub Action accepts multiple file globs.

### Documentation

- The Action's inputs are documented as a table.

## [0.8.1] - 2026-06-29

### Added

- Official pgsafe **GitHub Action** (composite) that runs the linter and posts inline PR annotations.

## [0.8.0] - 2026-06-29

### Added

- `--format github` — GitHub Actions workflow-annotation output.

## [0.7.0] - 2026-06-29

### Added

- `pgsafe --example-config` prints an annotated starter `pgsafe.toml`.

## [0.6.0] - 2026-06-28

### Changed

- Human output groups findings by statement so multiple findings on one statement no longer run together.

## [0.5.0] - 2026-06-29

### Added

- Opt-in policy lints, configurable via `pgsafe.toml`: `require-primary-key`, `require-not-null`, `require-comment`, `require-columns`, `require-if-exists`, `naming-convention`, `forbid-nullable-fk`, and `forbidden-column-type`.
- New hazard rules: `add-trigger`, `drop-constraint`, `identifier-too-long`, `fk-without-covering-index`, `attach-partition`, `detach-partition-non-concurrent`, `set-access-method`, and `enum-value-used-in-transaction`.
- `unchecked-do-block` (opt-in), plus static linting of the SQL inside `DO` blocks.
- `--version` flag.
- Prebuilt release binaries for Linux and macOS.

### Changed

- `alter-column-type` warns about cached-plan invalidation even for non-rewriting type changes.
- `rename` covers `ALTER TYPE … RENAME`; `require-if-exists` covers `CREATE MATERIALIZED VIEW` and `CREATE TABLE AS`.
- Config discovery also finds a non-dotfile `pgsafe.toml`.

## [0.4.0] - 2026-06-27

### Added

- `--git-diff` and `--since` changed-files selection — lint only the migrations that changed.
- `resolve()` front-end API seam that combines config and input selection.

## [0.3.0] - 2026-06-27

### Added

- `pgsafe.toml` configuration file — disable rules, override severities, set per-file options, and ignore paths with gitignore-style globs.
- New rules: `require-timeout` (engine-synthesized), `prefer-bigint-primary-key`, and `prefer-jsonb`.

## [0.2.0] - 2026-06-26

### Added

- `--in-transaction` flag and `LintOptions.assume_in_transaction`; `lint_sql` / `lint_input` now take `&LintOptions`.
- Rule-proving harness — DB-backed proofs that each rule's lock/rewrite claim holds against a real PostgreSQL server.

## [0.1.0] - 2026-06-26

Initial release.

### Added

- Postgres migration-safety linter with a rule engine, human and JSON output, stdin/file input, and CI-friendly exit codes.
- Library-first API (`lint`, `render`, `gate`) built with `forbid(unsafe_code)`.
- Initial rule set for lock- and rewrite-hazardous DDL: non-concurrent index build, foreign-key/check constraint without `NOT VALID`, `SET NOT NULL`, `ALTER COLUMN … TYPE`, renames, table/column/index drops, `TRUNCATE`, `VACUUM FULL` / `CLUSTER`, non-concurrent `REINDEX`, and `ADD COLUMN` with a volatile default, serial, identity, or stored-generated value, among others.
- Inline `pgsafe:ignore` suppression engine.
- `--fail-on` severity threshold to gate CI on findings.

[Unreleased]: https://github.com/fixed-width/pgsafe/compare/v0.10.0...HEAD
[0.10.0]: https://github.com/fixed-width/pgsafe/compare/v0.9.1...v0.10.0
[0.9.1]: https://github.com/fixed-width/pgsafe/compare/v0.9.0...v0.9.1
[0.9.0]: https://github.com/fixed-width/pgsafe/compare/v0.8.6...v0.9.0
[0.8.6]: https://github.com/fixed-width/pgsafe/compare/v0.8.5...v0.8.6
[0.8.5]: https://github.com/fixed-width/pgsafe/compare/v0.8.4...v0.8.5
[0.8.4]: https://github.com/fixed-width/pgsafe/compare/v0.8.3...v0.8.4
[0.8.3]: https://github.com/fixed-width/pgsafe/compare/v0.8.2...v0.8.3
[0.8.2]: https://github.com/fixed-width/pgsafe/compare/v0.8.1...v0.8.2
[0.8.1]: https://github.com/fixed-width/pgsafe/compare/v0.8.0...v0.8.1
[0.8.0]: https://github.com/fixed-width/pgsafe/compare/v0.7.0...v0.8.0
[0.7.0]: https://github.com/fixed-width/pgsafe/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/fixed-width/pgsafe/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/fixed-width/pgsafe/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/fixed-width/pgsafe/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/fixed-width/pgsafe/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/fixed-width/pgsafe/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/fixed-width/pgsafe/releases/tag/v0.1.0
