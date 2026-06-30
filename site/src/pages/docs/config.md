---
layout: ../../layouts/DocsLayout.astro
title: Configuration — pgsafe
description: Configure pgsafe with pgsafe.toml, suppress findings, and select which files to lint.
---

# Configuration

## `pgsafe.toml`

Drop a `pgsafe.toml` at your repo root to set defaults, turn rules off, change a rule's
severity, or ignore findings by path. pgsafe walks up from the current directory to the
nearest config file (stopping at the `.git` boundary). The hidden name `.pgsafe.toml` also
works; if a directory holds both, the plain `pgsafe.toml` wins. Every key is optional.

For a fully-annotated starting point covering every option, run `pgsafe --example-config`
(it prints to stdout): `pgsafe --example-config > pgsafe.toml`.

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

Several rules are opt-in **policy lints** enabled here (e.g. `require-primary-key`,
`naming-convention`, `forbidden-column-type`); see the [rules reference](/rules/) for each.

## Suppressing a single finding

When you have consciously accepted a finding — an index built in a maintenance window, a small
table where a rewrite is fine, a genuine false positive — suppress it inline with a directive
comment. A suppressed finding is still printed, but no longer affects the exit code.

```sql
-- pgsafe:ignore drop-table  superseded by v2, table confirmed empty
DROP TABLE legacy_events;

DROP TABLE old_audit;  -- pgsafe:ignore drop-table  one-off cleanup, off-peak
```

- Put the directive on the line **directly above** the statement, or **trailing** on the
  statement's own line — either way it binds to that one statement.
- Each directive silences **one** rule id. Stack directives one per line to silence several.
- **A reason is required.** It builds an audit trail and shows up in the PR diff.

Malformed or stale directives are reported (and gate CI) rather than silently ignored, so a
typo can never leave a real hazard un-suppressed (`suppression-malformed`,
`suppression-unknown-rule`, `suppression-missing-reason` are errors; `suppression-unused` is a
warning).

## Linting only new or changed migrations

To adopt pgsafe on a repo full of existing migrations without fixing them all first, lint only
the migrations added after a cutoff. They usually run in lexicographic filename order, so this
is a simple, git-free path comparison that works on any CI with any checkout depth.

```sh
pgsafe --since db/migrate/0042_last_legacy.sql db/migrate/*.sql
```

Set the cutoff **once** when you adopt pgsafe. You can also set it in the config so CI just
runs `pgsafe db/migrate/*.sql`:

```toml
since = "db/migrate/0042_last_legacy.sql"
```

If you'd rather select by git history, `--git-diff <ref>` lints the `.sql` files added/modified
versus a ref (plus untracked ones):

```sh
pgsafe --git-diff origin/main
pgsafe --git-diff origin/main db/migrate   # scope to a directory (relative to the repo root)
```

This requires the ref to be present in the checkout (a single `git fetch --depth=1 origin <branch>`
is enough — full history is **not** needed). `--since` and `--git-diff` can't be combined.
