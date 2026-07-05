---
layout: ../../layouts/DocsLayout.astro
title: Output formats — pgsafe
description: pgsafe's human, JSON, GitHub-annotation, and SARIF output, plus severity and gating.
---

# Output formats

`--format` selects how findings are printed: `human` (default), `json`, `github`, or `sarif`.

```sh
pgsafe migration.sql                  # human-readable (default)
pgsafe --format json migration.sql    # machine-readable
pgsafe --format github migration.sql  # GitHub Actions annotations
pgsafe --format sarif migration.sql   # SARIF 2.1.0, for GitHub code scanning
```

## JSON

The `--format json` output is a versioned envelope:

```json
{
  "schema_version": 2,
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

### The `fix` object

Some findings carry an optional `fix` object describing an unambiguous mechanical remediation —
for example, adding `CONCURRENTLY` to a `CREATE INDEX`. Advisory findings (`warning`-only
outcomes such as `DROP TABLE` or `RENAME`) never carry one.

```json
{
  "rule_id": "add-index-non-concurrent",
  "severity": "error",
  "message": "CREATE INDEX without CONCURRENTLY takes an AccessExclusiveLock ...",
  "fix": {
    "title": "Add CONCURRENTLY",
    "edits": [
      { "start": 12, "end": 12, "replacement": " CONCURRENTLY" }
    ]
  }
}
```

`start` and `end` are absolute UTF-8 byte offsets into the submitted SQL string.
`start == end` means a pure insertion (no bytes are removed).
The `edits` array is in ascending offset order and the ranges never overlap. Because each
edit's offsets reference the original SQL, a consumer can apply them in reverse (last to
first) without adjusting any offsets, or apply them in forward order while tracking the
cumulative length change.

The in-browser playground surfaces a **Fix** button on any finding that includes a `fix` object;
clicking it rewrites the editor content in place.

## Applying fixes

The CLI can apply a finding's fix directly, without going through JSON. Preview it as a
unified diff:

```sh
pgsafe --diff db/migrate/003_add_index.sql
```

The output is a standard unified diff, so it pipes straight into `git apply`:

```sh
pgsafe --diff db/migrate/003_add_index.sql | git apply
```

Or apply it — in place for a file, to stdout when reading from stdin:

```sh
pgsafe --fix db/migrate/003_add_index.sql
```

`--fix` and `--diff` are human-output only: they're mutually exclusive, and neither combines
with `--format json`, `--format github`, or `--format sarif`. A finding suppressed with
`-- pgsafe:ignore` is never auto-fixed. After `--fix`, the exit code reflects re-linting the
fixed file, per the [exit codes](/docs/ci/).

## SARIF (GitHub code scanning)

`--format sarif` emits SARIF 2.1.0, for upload to GitHub code scanning:

```yaml
- run: pgsafe --format sarif db/migrate/*.sql > pgsafe.sarif
- uses: github/codeql-action/upload-sarif@v3
  # pgsafe exits non-zero when findings gate, so upload the results regardless:
  if: always()
  with:
    sarif_file: pgsafe.sarif
```

Findings (including `-- pgsafe:ignore`-suppressed ones, marked dismissed via SARIF
`suppressions`) become SARIF results; a file that fails to parse becomes a tool-execution
notification instead of a result.

A findings run (exit 1) and a parse error (exit 2) both still write valid SARIF, so
`if: always()` uploads the results in the common cases. A configuration or I/O error
(e.g. an unreadable path) exits 2 *without* writing SARIF — the resulting 0-byte file
then (correctly) fails the upload. Pass repo-relative migration paths: absolute paths and
stdin don't map back to files GitHub can annotate as code-scanning alerts.

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
