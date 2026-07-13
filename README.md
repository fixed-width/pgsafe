# pgsafe

Static safety linter for PostgreSQL DDL migrations. `pgsafe` parses SQL and flags schema
changes likely to lock or break production — no database connection, no network.

**📖 Full documentation, the rules reference, and an in-browser playground:
[pgsafe.fixedwidth.tech](https://pgsafe.fixedwidth.tech)**

## Install

Install from [crates.io](https://crates.io/crates/pgsafe) with a Rust toolchain:

```sh
cargo install pgsafe
```

Or download a prebuilt binary from the [latest release](https://github.com/fixed-width/pgsafe/releases/latest)
(static Linux and macOS), or build from source with `cargo build --release` (binary at
`target/release/pgsafe`).

## Quickstart

```sh
pgsafe migration.sql                 # lint a file (exit 1 on a finding)
pgsafe --format json migration.sql   # machine-readable
pgsafe --list-rules                  # every rule this build checks
```

### Applying fixes

Findings that have an unambiguous mechanical rewrite carry a fix. Preview them:

```sh
pgsafe --diff db/migrate/003_add_index.sql
```

The output is a standard unified diff, so it pipes straight into `git apply`:

```sh
pgsafe --diff db/migrate/003_add_index.sql | git apply
```

Apply them in place (or to stdout when reading stdin):

```sh
pgsafe --fix db/migrate/003_add_index.sql
```

`--fix` and `--diff` are human-output only and cannot be combined with each other
or with `--format json`/`--format github`/`--format sarif`. A `-- pgsafe:ignore` finding is never
auto-fixed. After `--fix`, the exit code reflects re-linting the fixed file.

Run it in CI with the [GitHub Action](https://pgsafe.fixedwidth.tech/docs/ci/):

```yaml
- uses: fixed-width/pgsafe@v0.8.6
  with:
    files: "db/migrate/*.sql"
```

### SARIF output (GitHub code scanning)

`--format sarif` emits SARIF 2.1.0, for upload to GitHub code scanning:

```yaml
- run: pgsafe --format sarif db/migrate/*.sql > pgsafe.sarif
- uses: github/codeql-action/upload-sarif@v3
  # pgsafe exits non-zero when findings gate, so upload the results regardless:
  if: always()
  with:
    sarif_file: pgsafe.sarif
```

Findings (including `-- pgsafe:ignore`-suppressed ones, marked dismissed) become SARIF
results; a file that fails to parse becomes a tool-execution notification.

A findings run (exit 1) and a parse error (exit 2) both still write valid SARIF, so
`if: always()` uploads the results in the common cases. A configuration or I/O error
(e.g. an unreadable path) exits 2 *without* writing SARIF — the resulting 0-byte file
then (correctly) fails the upload. Pass repo-relative migration paths: absolute paths and
stdin don't map back to files GitHub can annotate as code-scanning alerts.

See [pgsafe.fixedwidth.tech/docs](https://pgsafe.fixedwidth.tech/docs/) for configuration,
output formats, and the full [rules reference](https://pgsafe.fixedwidth.tech/rules/).

## Editor integration (LSP)

`pgsafe lsp` starts a Language Server over stdio, giving any LSP-capable editor live
diagnostics on `.sql` files plus quickfix actions for findings that carry a safe rewrite.
It reads the same `pgsafe.toml` as the CLI, resolved per file and refreshed when the
config changes. If that config sets a top-level `paths` key, the server only lints files
matching those globs (relative to the config file) and offers no quickfixes for the rest —
useful for scoping to a `migrations/` directory and skipping schema dumps or ad-hoc query
files. With no `paths` key, every `.sql` file is linted.

The CLI honors the same `paths` key: a file you pass that doesn't match is skipped, and
piped stdin is never filtered. One `paths` setting scopes both surfaces.

The prebuilt release binaries include the language server. Installing from source, enable
the `lsp` Cargo feature (off by default):

```sh
cargo install pgsafe --features lsp
```

**Neovim** (built-in `vim.lsp`, Neovim 0.11+):

```lua
vim.lsp.config.pgsafe = {
  cmd = { "pgsafe", "lsp" },
  filetypes = { "sql" },
  root_markers = { "pgsafe.toml", ".pgsafe.toml", ".git" },
}
vim.lsp.enable("pgsafe")
```

**Helix** (`languages.toml`):

```toml
[language-server.pgsafe]
command = "pgsafe"
args = ["lsp"]

[[language]]
name = "sql"
language-servers = ["pgsafe"]
```

**Zed and other editors**: register `pgsafe lsp` as a custom stdio language server for
the `sql` language; see your editor's docs for where custom servers are configured.

## Changelog

Notable changes for each release are in [CHANGELOG.md](CHANGELOG.md).

## License

Apache-2.0. See [LICENSE](LICENSE).
