# pgsafe

Static safety linter for PostgreSQL DDL migrations.

`pgsafe` parses SQL migration files and flags schema changes that are likely to take
long-running locks or break running application code — before they reach production.
It requires no database connection and no network access.

## Install

Build from source (requires a Rust toolchain):

```sh
cargo build --release
# binary at target/release/pgsafe

# Or install into ~/.cargo/bin:
cargo install --path .
```

## Usage

```sh
# Lint a file
pgsafe migration.sql

# Lint multiple files
pgsafe 001.sql 002.sql

# Read from stdin
cat migration.sql | pgsafe -
pgsafe          # no args also reads stdin

# Machine-readable output for CI scripts
pgsafe --format json migration.sql

# Pipe into jq
pgsafe --format json migration.sql | jq '.files[].findings[] | select(.severity == "error")'
```

### JSON output shape

The `--format json` output is a versioned envelope:

```json
{
  "schema_version": 1,
  "files": [
    {
      "file": "migration.sql",
      "findings": [
        { "rule_id": "non-concurrent-index", "severity": "error", ... }
      ]
    }
  ]
}
```

If a file cannot be parsed, the `"findings"` array is empty and an `"error"` key is added to that file's object. Other files in the same run are still reported normally.

### Exit codes

| Code | Meaning |
|------|---------|
| 0 | No findings — migration looks safe |
| 1 | One or more findings (warnings or errors) |
| 2 | Any file failed to parse (or an I/O error occurred) |

This makes `pgsafe` straightforward to gate in CI:

```sh
pgsafe migrations/*.sql || exit 1
```

## Rules

| Rule ID | Description |
|---------|-------------|
| `non-concurrent-index` | `CREATE INDEX` without `CONCURRENTLY` blocks all writes for the duration of the build |
| `add-fk-without-not-valid` | Adding a foreign key without `NOT VALID` scans and locks both tables |
| `add-check-without-not-valid` | Adding a `CHECK` constraint without `NOT VALID` scans the whole table under a lock |
| `set-not-null` | `ALTER COLUMN ... SET NOT NULL` scans the entire table under an `ACCESS EXCLUSIVE` lock |
| `alter-column-type` | `ALTER COLUMN ... TYPE` usually rewrites the whole table and rebuilds indexes under a lock |
| `rename` | Renaming a table or column breaks existing queries and ORM mappings that reference the old name |

## Scope (v0)

`pgsafe` is a **static** analyzer: it parses SQL text only. It does not connect to a
database, inspect table sizes, or check runtime conditions. All findings are based on
what the SQL statement will do in the general case on a live production database.

Deeper checks — constraint validation state, column nullability, sequence ownership,
index concurrency on replicas — are planned for future versions.

## License

Apache-2.0. See [LICENSE](LICENSE).
