# pgsafe

Static safety linter for PostgreSQL DDL migrations. `pgsafe` parses SQL and flags schema
changes likely to lock or break production — no database connection, no network.

**📖 Full documentation, the rules reference, and an in-browser playground:
[pgsafe.fixedwidth.tech](https://pgsafe.fixedwidth.tech)**

## Install

Download a prebuilt binary from the [latest release](https://github.com/fixed-width/pgsafe/releases/latest)
(static Linux and macOS), or build from source:

```sh
cargo build --release   # target/release/pgsafe
```

## Quickstart

```sh
pgsafe migration.sql                 # lint a file (exit 1 on a finding)
pgsafe --format json migration.sql   # machine-readable
pgsafe --list-rules                  # every rule this build checks
```

Run it in CI with the [GitHub Action](https://pgsafe.fixedwidth.tech/docs/ci/):

```yaml
- uses: fixed-width/pgsafe@v0.8.5
  with:
    files: "db/migrate/*.sql"
```

See [pgsafe.fixedwidth.tech/docs](https://pgsafe.fixedwidth.tech/docs/) for configuration,
output formats, and the full [rules reference](https://pgsafe.fixedwidth.tech/rules/).

## License

Apache-2.0. See [LICENSE](LICENSE).
