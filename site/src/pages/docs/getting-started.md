---
layout: ../../layouts/DocsLayout.astro
title: Getting started — pgsafe
description: Install pgsafe and lint your first PostgreSQL migration.
---

# Getting started

pgsafe parses SQL migration files and flags schema changes likely to take long-running
locks or break running application code — before they reach production. It needs no
database connection and no network access.

## Install

Install from [crates.io](https://crates.io/crates/pgsafe) with a Rust toolchain:

```sh
cargo install pgsafe
```

Or download a prebuilt binary from the [latest release](https://github.com/fixed-width/pgsafe/releases/latest)
(static Linux and macOS builds), verify it against the matching `.sha256`, and put it on your `PATH`.
Either way, confirm it works:

```sh
pgsafe --version
```

You can also build from source with `cargo build --release` (binary at `target/release/pgsafe`).

## Lint a file

```sh
pgsafe migration.sql          # one file
pgsafe 001.sql 002.sql        # several
cat migration.sql | pgsafe -  # stdin
```

A clean migration exits `0`; any finding at or above the gate (default: any finding)
exits `1`; a parse or I/O error exits `2`.

See the [rules reference](/rules/) for every hazard pgsafe checks, and
[CI & GitHub Action](/docs/ci/) to gate migrations on every pull request.
