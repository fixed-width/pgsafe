# pgsafe

Static safety linter for PostgreSQL DDL migrations.

`pgsafe` parses SQL migration files and flags schema changes that are likely to take
long-running locks or break running application code — before they reach production.
It requires no database connection and no network access.

## Install

### Download a prebuilt binary

Each [release](https://github.com/fixed-width/pgsafe/releases/latest) attaches static, self-contained
Linux and macOS binaries. To install:

1. From the [latest release](https://github.com/fixed-width/pgsafe/releases/latest), download the
   archive for your platform:
   - Linux: `pgsafe-x86_64-unknown-linux-musl.tar.gz` or `pgsafe-aarch64-unknown-linux-musl.tar.gz`
   - macOS: `pgsafe-x86_64-apple-darwin.tar.gz` or `pgsafe-aarch64-apple-darwin.tar.gz`
2. Verify it against the matching `.sha256` file attached to the release.
3. Extract the archive and move the `pgsafe` binary onto your `PATH`.

Then `pgsafe --version` confirms the install.

### Build from source

Requires a Rust toolchain:

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
| `add-check-without-not-valid` | error | Adding a `CHECK` constraint without `NOT VALID` scans the whole table under a lock |
| `add-column-generated-stored` | error | Adding a `GENERATED ALWAYS AS (…) STORED` column computes the value for every existing row, rewriting the table under an `ACCESS EXCLUSIVE` lock |
| `add-column-identity` | error | Adding a `GENERATED … AS IDENTITY` column creates a sequence and rewrites every existing row under an `ACCESS EXCLUSIVE` lock |
| `add-column-not-null-no-default` | error | `ADD COLUMN ... NOT NULL` with no `DEFAULT` fails immediately on any non-empty table — it cannot fill existing rows |
| `add-column-serial` | error | Adding a `serial`/`bigserial` column creates a sequence and rewrites every existing row under an `ACCESS EXCLUSIVE` lock |
| `add-column-volatile-default` | error | Adding a column with a volatile `DEFAULT` (e.g. `random()`, `gen_random_uuid()`) rewrites every existing row under an `ACCESS EXCLUSIVE` lock |
| `add-exclusion-constraint` | error | Adding an `EXCLUDE` constraint builds an index under an `ACCESS EXCLUSIVE` lock, scanning the whole table |
| `add-fk-without-not-valid` | error | Adding a foreign key without `NOT VALID` scans and locks both tables |
| `add-index-non-concurrent` | error | `CREATE INDEX` without `CONCURRENTLY` blocks all writes for the duration of the build |
| `add-primary-key-without-index` | error | Adding a `PRIMARY KEY` inline builds its unique index (and may scan for `NOT NULL`) under an `ACCESS EXCLUSIVE` lock |
| `add-trigger` | warning | `CREATE TRIGGER` takes a `SHARE ROW EXCLUSIVE` lock and changes behavior for every subsequent write to the table |
| `add-unique-constraint` | error | Adding a `UNIQUE` constraint inline builds its underlying index while holding `ACCESS EXCLUSIVE` on the table for the whole build |
| `alter-column-type` | error | `ALTER COLUMN ... TYPE` usually rewrites the whole table under a lock; even a no-rewrite change (e.g. `varchar`→`text` or a precision widen) invalidates cached query plans and prepared statements (`cached plan must not change result type`) |
| `attach-partition` | warning | `ALTER TABLE … ATTACH PARTITION` locks the table being attached (`ACCESS EXCLUSIVE`) and scans it to validate the partition bound; add a matching validated `CHECK` first to skip the scan |
| `concurrently-in-transaction` | error | A `CREATE`/`DROP INDEX CONCURRENTLY` or `REINDEX … CONCURRENTLY` inside a transaction fails at runtime — Postgres rejects `CONCURRENTLY` in a transaction; use `--in-transaction` when the wrapper is implicit |
| `detach-partition-non-concurrent` | error | `ALTER TABLE … DETACH PARTITION` takes `ACCESS EXCLUSIVE` on the parent and the partition, blocking the whole partitioned table; use `… DETACH PARTITION … CONCURRENTLY` (PG 14+) |
| `drop-column` | warning | `DROP COLUMN` breaks any application code still referencing the column the moment it runs |
| `drop-constraint` | warning | `DROP CONSTRAINT` removes a foreign-key/check/unique/primary-key integrity guarantee and can break logical-replication replica identity |
| `drop-index-non-concurrent` | error | `DROP INDEX` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock on the table, blocking reads and writes while it runs |
| `drop-table` | warning | `DROP TABLE` permanently and irreversibly removes the table and all its data; in-flight queries against it fail immediately |
| `enum-value-used-in-transaction` | warning | `ALTER TYPE … ADD VALUE` then using that value in the same transaction fails at runtime (`unsafe use of new value`) |
| `fk-without-covering-index` | warning | A foreign key on a newly added column with no covering index makes every parent change scan and lock the child |
| `forbid-nullable-fk` | warning | **(opt-in)** A nullable foreign-key column in a `CREATE TABLE` — enable with `[rules] forbid-nullable-fk = true` |
| `forbidden-column-type` | warning | **(opt-in, `[forbidden-types]`)** A column whose type is in the configured forbidden set — e.g. ban `timestamp` in favor of `timestamptz` |
| `identifier-too-long` | warning | An identifier written longer than 63 bytes is silently truncated by PostgreSQL, so two names sharing a 63-byte prefix collide |
| `naming-convention` | warning | **(opt-in, `[naming]`)** An introduced name that doesn't match the configured regex for its kind (table/column/index/constraint/sequence/trigger/schema) |
| `prefer-bigint-primary-key` | warning | A small-integer primary key overflows (`smallint`/`smallserial` at ~32k rows, `int`/`serial` at ~2.1B); use `bigint`/`bigserial`/identity |
| `prefer-jsonb` | warning | A `json` column has no equality/ordering operators (`SELECT DISTINCT`, `GROUP BY`, `UNION`, `ORDER BY` fail); use `jsonb` |
| `refresh-matview-non-concurrent` | error | `REFRESH MATERIALIZED VIEW` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock and blocks all reads while it rebuilds (`WITH NO DATA`, which only empties the view, is not flagged) |
| `reindex-non-concurrent` | error | `REINDEX` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock on each index it rebuilds, blocking writes (and reads through that index) |
| `rename` | warning | Renaming a table, column, type, enum value, or other object breaks existing queries, views, and functions that reference the old name |
| `require-columns` | warning | **(opt-in, `required-columns`)** A `CREATE TABLE` missing a configured required column (e.g. `created_at`) — counts a later `ADD COLUMN` |
| `require-comment` | warning | **(opt-in)** A new table or column left without a `COMMENT` — enable with `[rules] require-comment = true` |
| `require-if-exists` | warning | **(opt-in)** A `CREATE TABLE/INDEX/SEQUENCE/SCHEMA/MATERIALIZED VIEW/TABLE … AS` without `IF NOT EXISTS`, or a `DROP` without `IF EXISTS` — enable with `[rules] require-if-exists = true` |
| `require-not-null` | warning | **(opt-in)** A `CREATE TABLE` with a column left nullable — enable with `[rules] require-not-null = true` |
| `require-primary-key` | warning | **(opt-in)** A `CREATE TABLE` the migration leaves without a primary key — enable with `[rules] require-primary-key = true` |
| `require-timeout` | warning | A blocking-lock statement (`ALTER TABLE`, `DROP TABLE`, non-`CONCURRENTLY` `DROP INDEX`, `TRUNCATE`, non-`CONCURRENTLY` index/refresh, `REINDEX`, `CLUSTER`, `VACUUM FULL`) runs with no `lock_timeout`/`statement_timeout` set — if it queues behind a slow query it blocks every query behind it |
| `set-access-method` | error | `ALTER TABLE … SET ACCESS METHOD` (PG 15+) rewrites the entire table and rebuilds its indexes under an `ACCESS EXCLUSIVE` lock when the access method changes |
| `set-logged-unlogged` | error | `ALTER TABLE … SET {LOGGED\|UNLOGGED}` rewrites the entire table and its indexes under an `ACCESS EXCLUSIVE` lock |
| `set-not-null` | error | `ALTER COLUMN ... SET NOT NULL` scans the entire table under an `ACCESS EXCLUSIVE` lock |
| `truncate` | warning | `TRUNCATE` takes an `ACCESS EXCLUSIVE` lock and irreversibly removes all rows; with `CASCADE` the lock propagates to every FK-referencing table |
| `unchecked-do-block` | warning | **(opt-in)** A `DO $$ … $$` block containing SQL pgsafe can't statically analyze — a dynamic `EXECUTE`, or a body that won't parse; enable with `[rules] unchecked-do-block = true` |
| `vacuum-full-cluster` | error | `VACUUM FULL` and `CLUSTER` rewrite the entire table under an `ACCESS EXCLUSIVE` lock — minutes to hours of blocked reads and writes, plus 2× disk |

By default `concurrently-in-transaction` detects explicit `BEGIN … COMMIT` blocks in the SQL.
Pass `--in-transaction` to also flag `CONCURRENTLY` operations when the transaction is applied
implicitly by the migration tool (Rails, Flyway, and similar) rather than written in the file.

`require-timeout` is also cross-statement: a `SET lock_timeout` (or `SET LOCAL` inside a transaction,
or `SET statement_timeout`) earlier in the file satisfies it for the statements that follow; `RESET`
or a value of `0` turns it back off. A blocking-lock operation against a table created empty earlier in
the same migration is not flagged.

`identifier-too-long` flags any identifier — table, column, constraint, index, or trigger name, a
rename target, or a reference — written longer than 63 bytes, which PostgreSQL silently truncates.

`fk-without-covering-index` is cross-statement and scoped to new columns: it flags a foreign key on a
column the migration creates or adds when no index built anywhere in the migration leads with that
column. A `CREATE INDEX` on the column (in any statement) clears it. Foreign keys on pre-existing
columns are out of scope for the static linter.

The partition rules cover the two `ALTER TABLE … PARTITION` hazards. `detach-partition-non-concurrent`
flags a `DETACH PARTITION` that lacks `CONCURRENTLY` — it takes `ACCESS EXCLUSIVE` on the whole
partitioned table; the PG 14+ `CONCURRENTLY` form takes only `SHARE UPDATE EXCLUSIVE`. `attach-partition`
flags `ATTACH PARTITION`, which locks the table being attached (`ACCESS EXCLUSIVE`) and scans it to
validate the partition bound; adding a matching, already-validated `CHECK` constraint first lets the
attach skip the scan. An attach of a child created empty earlier in the same migration is not flagged.

`set-access-method` flags `ALTER TABLE … SET ACCESS METHOD` (PG 15+): changing a table's access method
rewrites the whole table under an `ACCESS EXCLUSIVE` lock. The linter can't see the current access
method, so it flags every `SET ACCESS METHOD` (setting it to the table's current method is a no-op).

`rename` also covers the `ALTER TYPE … RENAME` forms — renaming a type, a composite-type attribute, or
an enum value. (`ALTER TYPE … ADD VALUE` is a different operation and is not flagged.)

`enum-value-used-in-transaction` is cross-statement: a newly added enum value cannot be used in the
transaction that added it, so it flags `ALTER TYPE … ADD VALUE 'v'` when `'v'` is used in a later
statement of the same transaction (`--in-transaction` covers tool-wrapped migrations). Adding the value
in its own migration, or using it only after the transaction commits, is not flagged.

**Policy lints (opt-in).** Some rules enforce team conventions rather than flag a hazard, so they are
**off by default** and enabled per-project in the config: `require-primary-key` flags a `CREATE TABLE`
the migration leaves without a primary key (counting a PK added by a later `ALTER TABLE … ADD PRIMARY
KEY` in the same migration; temp tables and partition children are exempt). Enable it with
`[rules] require-primary-key = true` (or `= "error"`).

`require-not-null` flags any column a `CREATE TABLE` leaves nullable. A primary-key, identity, or
serial column counts as non-null, as does a column made `NOT NULL` by a later `ALTER TABLE … ALTER
COLUMN … SET NOT NULL` in the same migration; temp tables and partition children are exempt. Enable it
with `[rules] require-not-null = true` (or `= "error"`).

`naming-convention` is a **parameterized** policy lint: configure a regex per identifier kind in a
`[naming]` section and it flags any name a migration introduces (in `CREATE`/`ALTER`/`RENAME`) that
doesn't match. A malformed pattern is a config error.

```toml
[naming]
table  = "^[a-z][a-z0-9_]*$"   # snake_case
index  = "^(ix|uq)_"
```

`forbidden-column-type` is a **parameterized** policy lint: configure a `[forbidden-types]` map of
disallowed type → suggested replacement, and it flags any column a migration introduces (in `CREATE
TABLE` or `ADD COLUMN`) whose type is forbidden. Types are matched through the PostgreSQL parser, so
`char` matches `char(10)`/`character`, and `timestamp` is distinct from `timestamptz`. Extension types
(`citext`, `hstore`, …) work too. A type the parser doesn't recognize simply matches nothing, so check
spelling — a misspelled type is silently inert rather than an error.

```toml
[forbidden-types]
timestamp = "timestamptz"   # require time zones
char      = "text"
money     = "numeric"
```

`require-if-exists` enforces idempotent DDL: it flags a `CREATE TABLE`, `CREATE INDEX`, `CREATE
SEQUENCE`, `CREATE SCHEMA`, `CREATE MATERIALIZED VIEW`, or `CREATE TABLE … AS` written without `IF NOT
EXISTS`, and any `DROP` written without `IF EXISTS`. (`SELECT … INTO` has no `IF NOT EXISTS` form, so
it is left alone.) Enable with `[rules] require-if-exists = true`.

`require-comment` enforces documentation: every new table and every new column — whether introduced by
`CREATE TABLE` or by `ALTER TABLE … ADD COLUMN` — must have a `COMMENT`. A `COMMENT ON TABLE`/`COMMENT ON
COLUMN` anywhere in the migration (cross-statement) satisfies it. Enable with
`[rules] require-comment = true`.

`require-columns` enforces that every `CREATE TABLE` includes a configured set of columns (a column
added by a later `ALTER TABLE … ADD COLUMN` in the same migration counts). Names are matched
case-insensitively against PostgreSQL's folding — the configured names are lowercased, so `Created_At`
matches a `created_at` column. A genuinely quoted, mixed-case column (`"CreatedAt"`) keeps its case and
is outside this rule's scope; don't list such a name. Configure the list:

```toml
required-columns = ["created_at", "updated_at"]
```

`forbid-nullable-fk` flags a foreign-key column a `CREATE TABLE` leaves nullable — inline `… REFERENCES`
columns and the columns of a table-level `FOREIGN KEY (…)`. A column that is `NOT NULL` (inline, via a
primary key, an identity column, a serial type, or a later `SET NOT NULL`) is not flagged. Enable with
`[rules] forbid-nullable-fk = true`.

pgsafe analyzes the static SQL inside `DO $$ … $$` blocks by default: statements PL/pgSQL parses
directly (e.g. an `ALTER TABLE` or `CREATE INDEX` in the body, including inside `IF`/`LOOP`/`CASE`) are
recovered and run through every **per-statement** rule, reported as `Inside a DO block: …`.
Cross-statement and synthesized rules (such as `require-timeout`, `fk-without-covering-index`,
`concurrently-in-transaction`) are not applied inside `DO` blocks. What pgsafe cannot see is
dynamic execution — `EXECUTE '…'`, especially when the string is built at runtime — and a body that
won't parse. The opt-in `unchecked-do-block` rule flags exactly that residue, so you know when a block
still holds SQL the linter couldn't check. Enable with `[rules] unchecked-do-block = true`.

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

- Put the directive on the line **directly above** the statement, or **trailing** on
  the statement's own line — either way it binds to that one statement.
- Each directive silences **one** rule id. To silence several rules on the same
  statement, stack the directives one per line on the lines just above it.
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

Drop a `pgsafe.toml` at your repo root to set defaults, turn rules off, change a rule's
severity, or ignore findings by path. pgsafe walks up from the current directory to the
nearest config file (stopping at the `.git` boundary). The hidden name `.pgsafe.toml` also
works; if a directory holds both, the plain `pgsafe.toml` wins. Every key is optional.

For a fully-annotated starting point covering every option, run `pgsafe --example-config`
(it prints to stdout): `pgsafe --example-config > .pgsafe.toml`.

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
**Discovery:** `--config <path>` uses an exact file; `--no-config` ignores any config file.
**Validation is strict:** an unknown key, an unknown rule id, a bad value, or a bad glob fails
the run (exit 2) rather than being silently ignored — so a typo can't quietly disable a check.

## GitHub Action

Lint a PR's changed migrations and get inline annotations on the diff:

```yaml
# .github/workflows/pgsafe.yml
on: pull_request
permissions:
  contents: read
  pull-requests: read
jobs:
  pgsafe:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v7
      - uses: fixed-width/pgsafe@v0.8.1
        with:
          files: 'db/migrate/*.sql'   # default: *.sql (any depth)
```

### Inputs

All inputs are optional.

| Input | Default | Description |
|---|---|---|
| `version` | the pinned ref | pgsafe release to download, e.g. `v0.8.1`. Defaults to the ref the action is pinned at; falls back to the latest release if that ref has no binary. |
| `files` | `*.sql` | Glob selecting which changed files to lint. `fnmatch` semantics — `*` spans `/`, so `*.sql` matches `.sql` at any depth and `db/migrate/*.sql` scopes to one tree. |
| `fail-on` | `warning` | Minimum severity that fails the check: `error`, `warning`, or `never`. |
| `config` | discovery | Path to a `.pgsafe.toml`. Empty uses pgsafe's own config discovery. |
| `working-directory` | `.` | Directory to lint from. |

The action needs `pull-requests: read` to read the PR's changed files through the GitHub API (no
special checkout depth required). Findings appear as inline annotations on the diff, and the check's
pass/fail follows `fail-on`.

## Linting only new migrations

To adopt pgsafe on a repo full of existing migrations without fixing them all first, lint only
the migrations added after a cutoff. They usually run in lexicographic filename order, so this is a
simple, git-free path comparison that works on any CI with any checkout depth.

```sh
# Lint only migrations whose path sorts after the last legacy one:
pgsafe --since db/migrate/0042_last_legacy.sql db/migrate/*.sql
```

Set the cutoff **once** when you adopt pgsafe — every new migration sorts after it and is linted,
every legacy one before it is skipped, and you never have to bump it. You can also set it in
`pgsafe.toml` so CI just runs `pgsafe db/migrate/*.sql`:

```toml
since = "db/migrate/0042_last_legacy.sql"
```

### Using git instead

If you'd rather select by git history, `--git-diff <ref>` lints the `.sql` files added/modified
versus a ref (plus untracked ones), so a PR checks only what it changed:

```sh
pgsafe --git-diff origin/main
pgsafe --git-diff origin/main db/migrate   # scope to a directory (relative to the repo root)
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
