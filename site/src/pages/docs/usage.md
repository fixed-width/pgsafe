---
layout: ../../layouts/DocsLayout.astro
title: Usage — pgsafe
description: The everyday loop — lint your migrations, read a finding, apply its fix or accept it, and gate the rest in CI.
---

# Usage

Day to day, pgsafe fits a tight loop: **lint** your migrations, **read** what it flags,
**apply** the suggested fix or **consciously accept** it, and **gate** the rest in CI so an
unsafe migration can't merge. This page walks that loop end to end; see
[Getting started](/docs/getting-started/) to install first.

## A worked example: fixing an unsafe index

Say `db/migrate/003_add_index.sql` adds an index the obvious way:

```sql
CREATE INDEX idx_users_email ON users (email);
```

Lint it:

```sh
pgsafe db/migrate/003_add_index.sql
```

pgsafe reports two findings on that one statement:

```
db/migrate/003_add_index.sql:1:1  CREATE INDEX idx_users_email ON users (email)
  error [add-index-non-concurrent]
    CREATE INDEX without CONCURRENTLY takes a lock that blocks writes to the table for the entire build.
    fix: Use CREATE INDEX CONCURRENTLY (outside a transaction block). A failed CONCURRENTLY build leaves an INVALID index: drop it with DROP INDEX CONCURRENTLY and retry, or rebuild with REINDEX INDEX CONCURRENTLY.
  warning [require-timeout]
    This statement takes a lock but no lock_timeout is set — if it queues behind a slow query, it blocks every query on the table until it acquires the lock.
    fix: Set a bounded lock_timeout first, e.g. `SET lock_timeout = '5s';` (or `SET LOCAL` inside a transaction), so the statement fails fast instead of piling up the lock queue. statement_timeout also satisfies this.
```

Each finding shows its severity and rule id in brackets — `error [add-index-non-concurrent]` —
followed by the hazard and a `fix:` hint. An **error** is an avoidable outage that a standard
rewrite prevents; a **warning** is an intentional or unavoidable heads-up. The
[rules reference](/rules/) documents every rule pgsafe checks.

### Preview the fix

Both findings carry a mechanical fix. Preview them as a unified diff before changing anything:

```sh
pgsafe --diff db/migrate/003_add_index.sql
```

```diff
--- a/db/migrate/003_add_index.sql
+++ b/db/migrate/003_add_index.sql
@@ -1,1 +1,2 @@
-CREATE INDEX idx_users_email ON users (email);
+SET lock_timeout = '5s';
+CREATE INDEX CONCURRENTLY idx_users_email ON users (email);
```

The output is a standard unified diff, so it pipes straight into `git apply`:

```sh
pgsafe --diff db/migrate/003_add_index.sql | git apply
```

### Apply the fix

Or let pgsafe apply it — in place for a file, or to stdout when reading from stdin:

```sh
pgsafe --fix db/migrate/003_add_index.sql
```

Re-lint to confirm the migration is now clean:

```sh
pgsafe db/migrate/003_add_index.sql   # no findings, exits 0
```

A few rules for `--fix` and `--diff`:

- They are **human-output only**: mutually exclusive with each other, and neither combines
  with `--format json`, `--format github`, or `--format sarif`.
- A finding suppressed with `-- pgsafe:ignore` is never auto-fixed.
- After `--fix`, the exit code reflects re-linting the fixed file — see the
  [exit codes](/docs/ci/).

Only findings with an unambiguous mechanical remediation are fixable; advisory findings such
as a `DROP TABLE` or a `RENAME` have no automatic fix — you decide what to do with those.

## Accept a finding you've reviewed

Sometimes a finding is one you've consciously accepted — an intentional `DROP TABLE`, an index
built in a maintenance window, or a genuine false positive. Suppress it inline with a directive
comment carrying a required reason:

```sql
-- pgsafe:ignore drop-table  superseded by v2, table confirmed empty
DROP TABLE legacy_events;
```

The finding is still printed but no longer affects the exit code. See
[Configuration](/docs/config/) for the full suppression rules — one rule id per directive,
where to place it, and how stale directives are caught so a typo can't hide a real hazard.

## Adopt on an existing repo

To introduce pgsafe on a repo full of migrations without fixing them all first, lint only the
migrations added after a cutoff:

```sh
pgsafe --since db/migrate/0042_last_legacy.sql db/migrate/*.sql
```

See [Configuration](/docs/config/) for setting the cutoff once, or selecting by git history
with `--git-diff`.

## Gate it in CI

Once your migrations are clean locally, gate every pull request so an unsafe migration can't
merge: pgsafe exits non-zero on a finding, and the GitHub Action annotates the diff inline.
See [CI & GitHub Action](/docs/ci/).
