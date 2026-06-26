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

## Suppressing a finding

When you have consciously accepted a finding — an index built in a maintenance
window, a small table where a rewrite is fine, a genuine false positive — suppress
it inline with a directive comment. A suppressed finding is still printed, but no
longer affects the exit code.

```sql
-- pgsafe:ignore drop-table  superseded by v2, table confirmed empty
DROP TABLE legacy_events;

DROP TABLE old_audit;  -- pgsafe:ignore drop-table  one-off cleanup, off-peak
```

- The directive must sit on the line(s) **immediately above** the statement, or
  **trailing** on the statement's own line.
- One rule id per directive; stack two directive lines to suppress two rules.
- **A reason is required.** It builds an audit trail and shows up in the PR diff.

Malformed or stale directives are reported (and gate CI) rather than silently
ignored, so a typo can never leave a real hazard un-suppressed:

| Diagnostic | Severity | When |
|------------|----------|------|
| `suppression-malformed` | error | unknown verb, or no rule id |
| `suppression-unknown-rule` | error | the rule id is not a real rule (typo) |
| `suppression-missing-reason` | error | the directive has no reason |
| `suppression-unused` | warning | the directive matched no finding (stale) |

## Scope (v0)

`pgsafe` is a **static** analyzer: it parses SQL text only. It does not connect to a
database, inspect table sizes, or check runtime conditions. All findings are based on
what the SQL statement will do in the general case on a live production database.

Deeper checks — constraint validation state, column nullability, sequence ownership,
index concurrency on replicas — are planned for future versions.

## Known limitations

Because `pgsafe` analyzes one statement at a time (v0), rules like
`add-unique-constraint`, `add-primary-key-without-index`, and
`add-column-not-null-no-default` will flag operations on a table that was
**created earlier in the same migration file**. In practice those operations
are safe — the table is empty and not yet visible to other sessions — but the
linter cannot tell. Cross-statement awareness (new-table suppression) is
planned for a future version.

## License

Apache-2.0. See [LICENSE](LICENSE).
