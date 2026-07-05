---
layout: ../../layouts/DocsLayout.astro
title: CI & GitHub Action — pgsafe
description: Gate pull requests on unsafe PostgreSQL migrations with the pgsafe GitHub Action.
---

# CI & GitHub Action

pgsafe is built to gate migrations in CI. It exits non-zero when a migration is unsafe, so
any CI can fail the build on it.

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
      - uses: fixed-width/pgsafe@v0.9.0
        with:
          files: 'db/migrate/*.sql'   # default: *.sql (any depth)
```

To lint more than one location, pass several globs (comma- or newline-separated); a file is
linted if it matches any of them:

```yaml
        with:
          files: |
            db/migrate/*.sql
            api/sql/*.sql
```

The action needs `pull-requests: read` to read the PR's changed files through the GitHub API
(no special checkout depth required). Findings appear as inline annotations on the diff, and
the check's pass/fail follows `fail-on`.

### Inputs

All inputs are optional.

| Input | Default | Description |
|---|---|---|
| `version` | the pinned ref | pgsafe release to download, e.g. `v0.8.1`. Falls back to the latest release if the pinned ref has no binary. |
| `files` | `*.sql` | One or more globs selecting which changed files to lint, comma- or newline-separated; linted if it matches any. `*` spans `/`, so `*.sql` matches any depth and `db/migrate/*.sql` scopes to one tree. |
| `fail-on` | `warning` | Minimum severity that fails the check: `error`, `warning`, or `never`. |
| `config` | discovery | Path to a `pgsafe.toml`. Empty uses pgsafe's own [config discovery](/docs/config/). |
| `working-directory` | `.` | Directory to lint from. |
| `verify-provenance` | `true` | Verify the binary's SLSA build provenance with `gh attestation verify` before use. Set `false` to pin a release built before provenance (pre-v0.8.3). |

Verification checks that the downloaded binary was built by this repository's release workflow.
If your runner's token cannot read the action repository's public attestations, add
`attestations: read` to the job's `permissions`.

## Code scanning (SARIF)

The GitHub Action above annotates the PR diff inline. To get **code-scanning alerts in the
Security tab** instead — persistent, and dismissible in the GitHub UI — upload pgsafe's SARIF
output:

```yaml
- run: pgsafe --format sarif db/migrate/*.sql > pgsafe.sarif
- uses: github/codeql-action/upload-sarif@v3
  # pgsafe exits non-zero when findings gate, so upload the results regardless:
  if: always()
  with:
    sarif_file: pgsafe.sarif
```

A findings run (exit 1) and a parse error (exit 2) both write valid SARIF, so `if: always()`
uploads the results in the common cases. A configuration or I/O error (e.g. an unreadable path)
exits 2 *without* writing SARIF — the resulting 0-byte file then (correctly) fails the upload.
Pass repo-relative migration paths: absolute paths and stdin don't map back to files GitHub can
annotate as code-scanning alerts.

See [Output formats](/docs/output/) for what pgsafe's SARIF contains (suppressions and
parse-error notifications).

## Any CI: gate on the exit code

pgsafe's exit code makes it easy to gate in any pipeline:

| Code | Meaning |
|------|---------|
| 0 | No findings — migration looks safe |
| 1 | One or more findings at or above `--fail-on` (default `warning`, i.e. any finding) |
| 2 | Any file failed to parse (or an I/O error occurred) |

```sh
pgsafe migrations/*.sql || exit 1
```

See [Output formats](/docs/output/) for `--format json`/`github`/`sarif`, and
[Configuration](/docs/config/) for selecting only changed or new migrations.
