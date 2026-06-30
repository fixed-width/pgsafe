---
layout: ../../layouts/DocsLayout.astro
title: Output formats — pgsafe
description: pgsafe's human, JSON, and GitHub-annotation output, plus severity and gating.
---

# Output formats

`--format` selects how findings are printed: `human` (default), `json`, or `github`.

```sh
pgsafe migration.sql                  # human-readable (default)
pgsafe --format json migration.sql    # machine-readable
pgsafe --format github migration.sql  # GitHub Actions annotations
```

## JSON

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

If a file cannot be parsed, its `"findings"` array is empty and an `"error"` key is added to
that file's object. Other files in the same run are still reported normally.

Pipe it into `jq` to filter:

```sh
pgsafe --format json migration.sql | jq '.files[].findings[] | select(.severity == "error")'
```

## Severity & gating

Each rule is `error` or `warning`:

- **`error`** — the statement takes a lock that blocks concurrent access, rewrites/validates the
  table, or fails outright, **and a standard rewrite avoids it** (`CONCURRENTLY`, `NOT VALID` →
  `VALIDATE`, `USING INDEX`, a two-step). These are the avoidable outages.
- **`warning`** — an intentional destructive op (`DROP TABLE`/`DROP COLUMN`/`TRUNCATE`), an
  app-compatibility heads-up (`RENAME`), or a schema-design issue (a `json` column, a small-int
  primary key) — cases where no lock-avoiding rewrite applies.

`--fail-on` controls which severities fail the run: `warning` (default — any finding fails),
`error` (only errors fail; warnings print but exit `0`), or `never` (report-only). Parse and
I/O errors always exit `2`, regardless of `--fail-on`. See the [exit codes](/docs/ci/) for
gating.
