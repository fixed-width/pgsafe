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

# Gate strictness (default: any finding fails the run)
pgsafe --fail-on=error migration.sql   # only error-severity findings fail (exit 1)
pgsafe --fail-on=never migration.sql   # report-only, never fails on findings

# Treat each migration as running inside a transaction (Rails, Flyway, and similar
# tools wrap each migration implicitly), so CONCURRENTLY index ops are
# flagged without an explicit BEGIN
pgsafe --in-transaction migration.sql
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
        { "rule_id": "add-index-non-concurrent", "severity": "error", ... }
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
| 1 | One or more findings at or above `--fail-on` (default `warning`, i.e. any finding) |
| 2 | Any file failed to parse (or an I/O error occurred) |

This makes `pgsafe` straightforward to gate in CI:

```sh
pgsafe migrations/*.sql || exit 1
```

## Rules

| Rule ID | Severity | Description |
|---------|----------|-------------|
| `add-index-non-concurrent` | error | `CREATE INDEX` without `CONCURRENTLY` blocks all writes for the duration of the build |
| `add-fk-without-not-valid` | error | Adding a foreign key without `NOT VALID` scans and locks both tables |
| `add-check-without-not-valid` | error | Adding a `CHECK` constraint without `NOT VALID` scans the whole table under a lock |
| `set-not-null` | error | `ALTER COLUMN ... SET NOT NULL` scans the entire table under an `ACCESS EXCLUSIVE` lock |
| `alter-column-type` | error | `ALTER COLUMN ... TYPE` usually rewrites the whole table and rebuilds indexes under a lock |
| `rename` | warning | Renaming a table or column breaks existing queries and ORM mappings that reference the old name |
| `drop-index-non-concurrent` | error | `DROP INDEX` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock on the table, blocking reads and writes while it runs |
| `drop-table` | warning | `DROP TABLE` permanently and irreversibly removes the table and all its data; in-flight queries against it fail immediately |
| `drop-column` | warning | `DROP COLUMN` breaks any application code still referencing the column the moment it runs |
| `truncate` | warning | `TRUNCATE` takes an `ACCESS EXCLUSIVE` lock and irreversibly removes all rows; with `CASCADE` the lock propagates to every FK-referencing table |
| `vacuum-full-cluster` | error | `VACUUM FULL` and `CLUSTER` rewrite the entire table under an `ACCESS EXCLUSIVE` lock — minutes to hours of blocked reads and writes, plus 2× disk |
| `reindex-non-concurrent` | error | `REINDEX` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock on each index it rebuilds, blocking writes (and reads through that index) |
| `add-unique-constraint` | error | Adding a `UNIQUE` constraint inline builds its underlying index while holding `ACCESS EXCLUSIVE` on the table for the whole build |
| `add-primary-key-without-index` | error | Adding a `PRIMARY KEY` inline builds its unique index (and may scan for `NOT NULL`) under an `ACCESS EXCLUSIVE` lock |
| `add-column-not-null-no-default` | error | `ADD COLUMN ... NOT NULL` with no `DEFAULT` fails immediately on any non-empty table — it cannot fill existing rows |
| `add-column-volatile-default` | error | Adding a column with a volatile `DEFAULT` (e.g. `random()`, `gen_random_uuid()`) rewrites every existing row under an `ACCESS EXCLUSIVE` lock |
| `add-column-serial` | error | Adding a `serial`/`bigserial` column creates a sequence and rewrites every existing row under an `ACCESS EXCLUSIVE` lock |
| `add-column-identity` | error | Adding a `GENERATED … AS IDENTITY` column creates a sequence and rewrites every existing row under an `ACCESS EXCLUSIVE` lock |
| `add-column-generated-stored` | error | Adding a `GENERATED ALWAYS AS (…) STORED` column computes the value for every existing row, rewriting the table under an `ACCESS EXCLUSIVE` lock |
| `set-logged-unlogged` | error | `ALTER TABLE … SET {LOGGED\|UNLOGGED}` rewrites the entire table and its indexes under an `ACCESS EXCLUSIVE` lock |
| `refresh-matview-non-concurrent` | error | `REFRESH MATERIALIZED VIEW` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock and blocks all reads while it rebuilds |
| `add-exclusion-constraint` | error | Adding an `EXCLUDE` constraint builds an index under an `ACCESS EXCLUSIVE` lock, scanning the whole table |
| `prefer-jsonb` | warning | A `json` column has no equality/ordering operators (`SELECT DISTINCT`/`GROUP BY` fail); use `jsonb` |
| `prefer-bigint-primary-key` | warning | An `int`/`serial` primary key overflows at ~2.1B rows; use `bigint`/`bigserial`/identity |
| `concurrently-in-transaction` | error | A `CREATE`/`DROP INDEX CONCURRENTLY` or `REINDEX … CONCURRENTLY` inside a transaction fails at runtime — Postgres rejects `CONCURRENTLY` in a transaction; use `--in-transaction` when the wrapper is implicit |
| `require-timeout` | warning | A blocking-lock statement (`ALTER TABLE`, `DROP`, `TRUNCATE`, non-`CONCURRENTLY` index/refresh, `REINDEX`, `CLUSTER`, `VACUUM FULL`) runs with no `lock_timeout`/`statement_timeout` set — if it queues behind a slow query it blocks every query behind it |

By default `concurrently-in-transaction` detects explicit `BEGIN … COMMIT` blocks in the SQL.
Pass `--in-transaction` to also flag `CONCURRENTLY` operations when the transaction is applied
implicitly by the migration tool (Rails, Flyway, and similar) rather than written in the file.

`require-timeout` is also cross-statement: a `SET lock_timeout` (or `SET LOCAL` inside a transaction,
or `SET statement_timeout`) earlier in the file satisfies it for the statements that follow; `RESET`
or a value of `0` turns it back off. A blocking-lock operation against a table created empty earlier in
the same migration is not flagged.

## Severity & gating

Each rule is `error` or `warning`:

- **`error`** — the statement takes a lock that blocks concurrent access, rewrites/validates the
  table, or fails outright, **and a standard rewrite avoids it** (`CONCURRENTLY`, `NOT VALID` →
  `VALIDATE`, `USING INDEX`, a two-step). These are the avoidable outages.
- **`warning`** — an intentional destructive op (`DROP TABLE` / `DROP COLUMN` / `TRUNCATE`), an
  app-compatibility heads-up (`RENAME`), or a schema-design issue (a `json` column, a small-int
  primary key) — cases where no lock-avoiding rewrite applies.

`--fail-on` controls which severities fail the run: `warning` (default — any finding fails),
`error` (only errors fail; warnings are printed but exit `0`), or `never` (report-only). Parse
and I/O errors always exit `2`, regardless of `--fail-on`.

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

## Configuration

Drop a `.pgsafe.toml` at your repo root to set defaults, turn rules off, change a rule's
severity, or ignore findings by path. pgsafe walks up from the current directory to the
nearest `.pgsafe.toml` (stopping at the `.git` boundary). Every key is optional.

```toml
# Default flags (an explicit CLI flag still wins over these).
fail_on        = "warning"   # "warning" | "error" | "never"
in_transaction = false
format         = "human"     # "human" | "json"

# Per-rule: disable, or force a severity.
[rules]
drop-table               = false       # turn the rule off
add-index-non-concurrent = "warning"   # report it, but as a warning

# Ignore findings for matching files (gitignore-style globs, relative to this file).
[[ignore]]
path  = "db/legacy/**"       # `rules` omitted ⇒ ignore everything here
[[ignore]]
path  = "db/vendor/**"
rules = ["drop-table"]       # ignore only these rules here
```

**Precedence:** an explicit CLI flag beats the config file, which beats the built-in default.
**Discovery:** `--config <path>` uses an exact file; `--no-config` ignores any `.pgsafe.toml`.
**Validation is strict:** an unknown key, an unknown rule id, a bad value, or a bad glob fails
the run (exit 2) rather than being silently ignored — so a typo can't quietly disable a check.

## Linting only new migrations

To adopt pgsafe on a repo full of existing migrations without fixing them all first, lint only
the migrations added after a cutoff. Migrations run in lexicographic filename order, so this is a
simple, git-free path comparison that works on any CI with any checkout depth.

```sh
# Lint only migrations whose path sorts after the last legacy one:
pgsafe --since db/migrate/0042_last_legacy.sql db/migrate/*.sql
```

Set the cutoff **once** when you adopt pgsafe — every new migration sorts after it and is linted,
every legacy one before it is skipped, and you never have to bump it. You can also set it in
`.pgsafe.toml` so CI just runs `pgsafe db/migrate/*.sql`:

```toml
since = "db/migrate/0042_last_legacy.sql"
```

### Using git instead

If you'd rather select by git history, `--git-diff <ref>` lints the `.sql` files added/modified
versus a ref (plus untracked ones), so a PR checks only what it changed:

```sh
pgsafe --git-diff origin/main
pgsafe --git-diff origin/main db/migrate   # scope to a directory
```

This requires the ref to be present in the checkout (a single `git fetch --depth=1 origin <branch>`
is enough — full history is **not** needed). `--since` and `--git-diff` can't be combined.

## Scope

`pgsafe` is a **static** analyzer: it parses SQL text only. It does not connect to a
database, inspect table sizes, or check runtime conditions. All findings are based on
what the SQL statement will do in the general case on a live production database.

Deeper checks — constraint validation state, column nullability, sequence ownership,
index concurrency on replicas — are planned for future versions.

## Known limitations

`pgsafe` recognizes a table `CREATE`d earlier in the same input and does not flag safe operations
on it **while it is still empty** — e.g. `CREATE TABLE foo (…); ALTER TABLE foo ADD CONSTRAINT … UNIQUE (…)`
is not flagged. Two deliberate caveats: matching is by exact name, so a schema-qualified mismatch
(`CREATE TABLE public.foo; ALTER TABLE foo …`) is still flagged conservatively; and once the table is
populated (`INSERT` / `COPY … FROM`) it is treated as an existing table and operations on it are
flagged again.

## License

Apache-2.0. See [LICENSE](LICENSE).
