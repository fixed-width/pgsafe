import catalog from "./rules.catalog.json";

export type Severity = "error" | "warning";
export type Category =
  | "Locking & rewrites"
  | "Constraints"
  | "Destructive"
  | "Schema design"
  | "Policy";

export interface RuleDoc {
  id: string;
  title: string;
  severity: Severity;
  category: Category;
  /** One line for the index table. */
  summary: string;
  /** Why it's unsafe — the lock taken or what breaks. Sourced from the rule's finding message. */
  whyUnsafe: string;
  /** The safe way to make the same change. Sourced from the rule's finding guidance. */
  safeRewrite: string;
  /** Optional worked example. */
  example?: { unsafe: string; safe?: string };
  /** Related rule ids. */
  related?: string[];
}

export const RULES: Record<string, RuleDoc> = {
  "add-index-non-concurrent": {
    id: "add-index-non-concurrent",
    title: "CREATE INDEX without CONCURRENTLY",
    severity: "error",
    category: "Locking & rewrites",
    summary: "Building an index without `CONCURRENTLY` blocks all writes for the whole build.",
    whyUnsafe:
      "`CREATE INDEX` takes a lock that blocks writes to the table for the entire build. On a large table that can be minutes of blocked writes.",
    safeRewrite:
      "Use `CREATE INDEX CONCURRENTLY` (outside a transaction block). A failed `CONCURRENTLY` build leaves an `INVALID` index: drop it with `DROP INDEX CONCURRENTLY` and retry, or rebuild with `REINDEX INDEX CONCURRENTLY`.",
    example: {
      unsafe: "CREATE INDEX idx_users_email ON users (email);",
      safe: "CREATE INDEX CONCURRENTLY idx_users_email ON users (email);",
    },
    related: ["reindex-non-concurrent", "drop-index-non-concurrent"],
  },
  "drop-column": {
    id: "drop-column",
    title: "DROP COLUMN",
    severity: "warning",
    category: "Destructive",
    summary: "Dropping a column breaks any app code still referencing it the moment it runs.",
    whyUnsafe:
      "`DROP COLUMN` breaks any application code still selecting or writing the column the moment it runs — and the data is gone.",
    safeRewrite:
      "Ship the code that stops using the column first; drop it in a later migration once nothing references it. Consider a two-phase deploy.",
    example: { unsafe: "ALTER TABLE users DROP COLUMN legacy_flag;" },
    related: ["rename", "drop-table"],
  },
  "add-fk-without-not-valid": {
    id: "add-fk-without-not-valid",
    title: "ADD FOREIGN KEY without NOT VALID",
    severity: "error",
    category: "Constraints",
    summary: "Adding a foreign key without `NOT VALID` scans and locks both tables.",
    whyUnsafe:
      "Adding a `FOREIGN KEY` without `NOT VALID` validates every existing row while holding locks on both tables.",
    safeRewrite:
      "Add the constraint with `NOT VALID` first (brief lock, no scan), then run `ALTER TABLE ... VALIDATE CONSTRAINT` in a separate statement (it allows concurrent reads and writes).",
    example: {
      unsafe:
        "ALTER TABLE orders ADD CONSTRAINT fk_customer FOREIGN KEY (customer_id) REFERENCES customers (id);",
      safe: "ALTER TABLE orders ADD CONSTRAINT fk_customer FOREIGN KEY (customer_id) REFERENCES customers (id) NOT VALID;\nALTER TABLE orders VALIDATE CONSTRAINT fk_customer;",
    },
    related: ["fk-without-covering-index", "add-check-without-not-valid"],
  },
  "add-check-without-not-valid": {
    id: "add-check-without-not-valid",
    title: "ADD CHECK without NOT VALID",
    severity: "error",
    category: "Constraints",
    summary: "Adding a `CHECK` constraint without `NOT VALID` scans the whole table under a lock.",
    whyUnsafe:
      "Adding a `CHECK` constraint without `NOT VALID` scans the whole table under an `ACCESS EXCLUSIVE` lock.",
    safeRewrite:
      "Add the `CHECK` with `NOT VALID`, then run `VALIDATE CONSTRAINT` separately (`SHARE UPDATE EXCLUSIVE` — concurrent reads and writes are allowed).",
    example: {
      unsafe: "ALTER TABLE orders ADD CONSTRAINT chk_total CHECK (total > 0);",
      safe: "ALTER TABLE orders ADD CONSTRAINT chk_total CHECK (total > 0) NOT VALID;\nALTER TABLE orders VALIDATE CONSTRAINT chk_total;",
    },
    related: ["add-fk-without-not-valid", "set-not-null"],
  },
  "set-not-null": {
    id: "set-not-null",
    title: "SET NOT NULL",
    severity: "error",
    category: "Locking & rewrites",
    summary: "`ALTER COLUMN ... SET NOT NULL` scans the entire table under an `ACCESS EXCLUSIVE` lock.",
    whyUnsafe:
      "`ALTER COLUMN ... SET NOT NULL` scans the entire table under an `ACCESS EXCLUSIVE` lock.",
    safeRewrite:
      "On PG12+, first add `CHECK (col IS NOT NULL) NOT VALID`, run `VALIDATE CONSTRAINT`, then `SET NOT NULL` (it reuses the validated check and skips the scan). Drop the helper `CHECK` afterward if you like.",
    example: {
      unsafe: "ALTER TABLE users ALTER COLUMN email SET NOT NULL;",
      safe: "ALTER TABLE users ADD CONSTRAINT users_email_nn CHECK (email IS NOT NULL) NOT VALID;\nALTER TABLE users VALIDATE CONSTRAINT users_email_nn;\nALTER TABLE users ALTER COLUMN email SET NOT NULL;",
    },
    related: ["add-column-not-null-no-default", "add-check-without-not-valid"],
  },
  "alter-column-type": {
    id: "alter-column-type",
    title: "ALTER COLUMN ... TYPE",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "`ALTER COLUMN ... TYPE` usually rewrites the table under a lock and invalidates cached query plans.",
    whyUnsafe:
      "`ALTER COLUMN ... TYPE` usually rewrites the whole table and rebuilds its indexes under an `ACCESS EXCLUSIVE` lock. Even a metadata-only change that does not rewrite (widening a `varchar`/`numeric`/`timestamp` precision, or `varchar`->`text`) changes the column's result type and breaks cached query plans and prepared statements in live sessions ('`cached plan must not change result type`') until they re-plan.",
    safeRewrite:
      "Use expand/contract for a rewriting change: add a new column, dual-write and backfill in batches, then swap (some changes, e.g. `int`->`bigint`, always rewrite). A no-rewrite change (e.g. `varchar`->`text` or widening a `varchar`) avoids the table rewrite but still invalidates cached plans, so recycle pooled connections or run `DISCARD PLANS` afterward, or apply it during a deploy window.",
    example: {
      unsafe: "ALTER TABLE events ALTER COLUMN id TYPE bigint;",
    },
    related: ["prefer-bigint-primary-key"],
  },
  rename: {
    id: "rename",
    title: "RENAME",
    severity: "warning",
    category: "Destructive",
    summary:
      "Renaming a table, column, type, or enum value breaks existing queries, views, and functions.",
    whyUnsafe:
      "Renaming this column breaks every application query, view, and function that references the old name.",
    safeRewrite:
      "Avoid renames in a rolling deploy. Prefer expand/contract: add the new name, dual-write, migrate readers, then drop the old name — or use a view to alias during the transition.",
    example: { unsafe: "ALTER TABLE users RENAME COLUMN email TO email_address;" },
    related: ["drop-column"],
  },
  "drop-index-non-concurrent": {
    id: "drop-index-non-concurrent",
    title: "DROP INDEX without CONCURRENTLY",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "`DROP INDEX` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock, blocking reads and writes.",
    whyUnsafe:
      "`DROP INDEX` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock on the index's table, blocking reads and writes while it runs.",
    safeRewrite: "Use `DROP INDEX CONCURRENTLY` (outside a transaction block).",
    example: {
      unsafe: "DROP INDEX idx_users_email;",
      safe: "DROP INDEX CONCURRENTLY idx_users_email;",
    },
    related: ["add-index-non-concurrent", "reindex-non-concurrent"],
  },
  "drop-table": {
    id: "drop-table",
    title: "DROP TABLE",
    severity: "warning",
    category: "Destructive",
    summary:
      "`DROP TABLE` permanently removes the table and all its data; in-flight queries fail immediately.",
    whyUnsafe:
      "`DROP TABLE` permanently and irreversibly removes the table and all its data; in-flight queries against it fail immediately.",
    safeRewrite:
      "Confirm all application references are retired and the table is traffic-free before dropping; archive the data first if it may be needed.",
    example: { unsafe: "DROP TABLE legacy_audit;" },
    related: ["drop-column", "truncate"],
  },
  truncate: {
    id: "truncate",
    title: "TRUNCATE",
    severity: "warning",
    category: "Destructive",
    summary: "`TRUNCATE` takes an `ACCESS EXCLUSIVE` lock and irreversibly removes all rows.",
    whyUnsafe:
      "`TRUNCATE` takes an `ACCESS EXCLUSIVE` lock and irreversibly removes all rows; with `CASCADE` the lock propagates to every FK-referencing table.",
    safeRewrite:
      "For ongoing data removal use a batched `DELETE`; reserve `TRUNCATE` for environments where downtime and data loss are acceptable.",
    example: { unsafe: "TRUNCATE users;" },
    related: ["drop-table"],
  },
  "vacuum-full-cluster": {
    id: "vacuum-full-cluster",
    title: "VACUUM FULL / CLUSTER",
    severity: "error",
    category: "Locking & rewrites",
    summary: "`VACUUM FULL` and `CLUSTER` rewrite the whole table under an `ACCESS EXCLUSIVE` lock.",
    whyUnsafe:
      "`VACUUM FULL` and `CLUSTER` rewrite the entire table under an `ACCESS EXCLUSIVE` lock — minutes to hours of blocked reads and writes, plus 2x disk.",
    safeRewrite:
      "Use pg_repack for online table/space maintenance; plain `VACUUM` (no `FULL`) takes only `SHARE UPDATE EXCLUSIVE` and allows concurrent reads and writes.",
    example: { unsafe: "VACUUM FULL users;" },
    related: ["reindex-non-concurrent"],
  },
  "reindex-non-concurrent": {
    id: "reindex-non-concurrent",
    title: "REINDEX without CONCURRENTLY",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "`REINDEX` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock on each index it rebuilds.",
    whyUnsafe:
      "`REINDEX` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock on each index it rebuilds, blocking writes (and reads through that index).",
    safeRewrite:
      "Use `REINDEX INDEX CONCURRENTLY` (PG12+, outside a transaction); on older servers use pg_repack or a maintenance window.",
    example: {
      unsafe: "REINDEX INDEX idx_users_email;",
      safe: "REINDEX INDEX CONCURRENTLY idx_users_email;",
    },
    related: ["add-index-non-concurrent", "drop-index-non-concurrent"],
  },
  "add-unique-constraint": {
    id: "add-unique-constraint",
    title: "ADD UNIQUE constraint inline",
    severity: "error",
    category: "Constraints",
    summary:
      "Adding a `UNIQUE` constraint inline builds its index while holding `ACCESS EXCLUSIVE` for the whole build.",
    whyUnsafe:
      "Adding a `UNIQUE` constraint inline builds its underlying index while holding `ACCESS EXCLUSIVE` on the table for the whole build.",
    safeRewrite:
      "Build the index first with `CREATE UNIQUE INDEX CONCURRENTLY`, then attach it: `ALTER TABLE ... ADD CONSTRAINT ... UNIQUE USING INDEX idx` (a brief lock).",
    example: {
      unsafe: "ALTER TABLE users ADD CONSTRAINT uq_users_email UNIQUE (email);",
      safe: "CREATE UNIQUE INDEX CONCURRENTLY uq_users_email ON users (email);\nALTER TABLE users ADD CONSTRAINT uq_users_email UNIQUE USING INDEX uq_users_email;",
    },
    related: ["add-primary-key-without-index", "add-index-non-concurrent"],
  },
  "add-primary-key-without-index": {
    id: "add-primary-key-without-index",
    title: "ADD PRIMARY KEY without a prebuilt index",
    severity: "error",
    category: "Constraints",
    summary:
      "Adding a `PRIMARY KEY` inline builds its unique index under an `ACCESS EXCLUSIVE` lock.",
    whyUnsafe:
      "Adding a `PRIMARY KEY` inline builds its unique index (and may scan for `NOT NULL`) under an `ACCESS EXCLUSIVE` lock.",
    safeRewrite:
      "Build the index with `CREATE UNIQUE INDEX CONCURRENTLY`, then attach it: `ALTER TABLE ... ADD CONSTRAINT ... PRIMARY KEY USING INDEX idx`.",
    example: {
      unsafe: "ALTER TABLE users ADD PRIMARY KEY (id);",
      safe: "CREATE UNIQUE INDEX CONCURRENTLY users_pkey ON users (id);\nALTER TABLE users ADD CONSTRAINT users_pkey PRIMARY KEY USING INDEX users_pkey;",
    },
    related: ["add-unique-constraint", "require-primary-key"],
  },
  "add-column-not-null-no-default": {
    id: "add-column-not-null-no-default",
    title: "ADD COLUMN NOT NULL without a default",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "`ADD COLUMN ... NOT NULL` with no `DEFAULT` fails immediately on any non-empty table.",
    whyUnsafe:
      "`ADD COLUMN ... NOT NULL` with no `DEFAULT` fails immediately on any non-empty table — it cannot fill existing rows.",
    safeRewrite:
      "Add the column nullable, backfill in batches, then enforce `NOT NULL` via `CHECK (col IS NOT NULL) NOT VALID` + `VALIDATE CONSTRAINT`, then `SET NOT NULL` (PG12+ reuses the validated check and skips the scan).",
    example: {
      unsafe: "ALTER TABLE users ADD COLUMN status text NOT NULL;",
      safe: "ALTER TABLE users ADD COLUMN status text;\n-- backfill in batches, then add NOT NULL via the safe two-step",
    },
    related: ["add-column-volatile-default", "set-not-null"],
  },
  "add-column-volatile-default": {
    id: "add-column-volatile-default",
    title: "ADD COLUMN with a volatile default",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "Adding a column with a volatile `DEFAULT` rewrites every existing row under an `ACCESS EXCLUSIVE` lock.",
    whyUnsafe:
      "`ADD COLUMN` with a volatile `DEFAULT` (e.g. `random()`, `gen_random_uuid()`) rewrites every existing row under an `ACCESS EXCLUSIVE` lock.",
    safeRewrite:
      "Add the column nullable with no default, backfill existing rows in batches, then `ALTER COLUMN ... SET DEFAULT` for new rows (add `NOT NULL` via the safe two-step if needed).",
    example: {
      unsafe: "ALTER TABLE users ADD COLUMN token uuid DEFAULT gen_random_uuid();",
      safe: "ALTER TABLE users ADD COLUMN token uuid;\n-- backfill in batches, then: ALTER TABLE users ALTER COLUMN token SET DEFAULT gen_random_uuid();",
    },
    related: ["add-column-not-null-no-default", "add-column-generated-stored"],
  },
  "add-column-serial": {
    id: "add-column-serial",
    title: "ADD COLUMN serial",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "Adding a `serial`/`bigserial` column creates a sequence and rewrites every row under an `ACCESS EXCLUSIVE` lock.",
    whyUnsafe:
      "`ADD COLUMN` with a `serial` type (e.g. `bigserial`) creates a sequence and rewrites every existing row under an `ACCESS EXCLUSIVE` lock.",
    safeRewrite:
      "Add a plain nullable integer column (e.g. `bigint`), create the sequence and backfill existing rows in batches, then `ALTER COLUMN ... SET DEFAULT nextval(...)` and add `NOT NULL` via the safe two-step — do not add `serial` directly to a populated table.",
    example: {
      unsafe: "ALTER TABLE users ADD COLUMN seq serial;",
      safe: [
        "-- add a plain nullable column (no rewrite), then a sequence default for new rows",
        "ALTER TABLE users ADD COLUMN seq bigint;",
        "CREATE SEQUENCE users_seq_seq OWNED BY users.seq;",
        "ALTER TABLE users ALTER COLUMN seq SET DEFAULT nextval('users_seq_seq');",
        "-- backfill existing rows in batches, then add NOT NULL via the safe two-step",
      ].join("\n"),
    },
    related: ["add-column-identity", "prefer-bigint-primary-key"],
  },
  "add-column-identity": {
    id: "add-column-identity",
    title: "ADD COLUMN GENERATED AS IDENTITY",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "Adding a `GENERATED ... AS IDENTITY` column creates a sequence and rewrites every row under an `ACCESS EXCLUSIVE` lock.",
    whyUnsafe:
      "`ADD COLUMN ... GENERATED AS IDENTITY` creates a sequence and rewrites every existing row under an `ACCESS EXCLUSIVE` lock.",
    safeRewrite:
      "Add a plain nullable integer column, backfill existing rows in batches, then attach the identity/sequence — do not add an identity column directly to a populated table.",
    example: {
      unsafe: "ALTER TABLE users ADD COLUMN n int GENERATED ALWAYS AS IDENTITY;",
      safe: [
        "-- add a plain nullable column (no rewrite)",
        "ALTER TABLE users ADD COLUMN n bigint;",
        "-- backfill existing rows in batches and set NOT NULL (safe two-step), then:",
        "ALTER TABLE users ALTER COLUMN n ADD GENERATED ALWAYS AS IDENTITY;",
      ].join("\n"),
    },
    related: ["add-column-serial"],
  },
  "add-column-generated-stored": {
    id: "add-column-generated-stored",
    title: "ADD COLUMN GENERATED ... STORED",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "Adding a `GENERATED ALWAYS AS (…) STORED` column rewrites the table under an `ACCESS EXCLUSIVE` lock.",
    whyUnsafe:
      "`ADD COLUMN ... GENERATED ALWAYS AS (...) STORED` computes and writes the value for every existing row, rewriting the table under an `ACCESS EXCLUSIVE` lock.",
    safeRewrite:
      "Add a plain nullable column, backfill the computed value in batches, and keep it current with a trigger or in application code — do not add a `STORED` generated column directly to a populated table.",
    example: {
      unsafe:
        "ALTER TABLE users ADD COLUMN full_name text GENERATED ALWAYS AS (first || ' ' || last) STORED;",
      safe: [
        "-- add a plain nullable column (no rewrite)",
        "ALTER TABLE users ADD COLUMN full_name text;",
        "-- backfill in batches: UPDATE users SET full_name = first || ' ' || last;",
        "-- keep it current with a trigger (or compute it in application code)",
      ].join("\n"),
    },
    related: ["add-column-volatile-default"],
  },
  "set-logged-unlogged": {
    id: "set-logged-unlogged",
    title: "SET LOGGED / UNLOGGED",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "`ALTER TABLE ... SET LOGGED/UNLOGGED` rewrites the entire table and its indexes under an `ACCESS EXCLUSIVE` lock.",
    whyUnsafe:
      "`ALTER TABLE ... SET LOGGED/UNLOGGED` rewrites the entire table and its indexes under an `ACCESS EXCLUSIVE` lock.",
    safeRewrite:
      "There is no online alternative — toggling durability rewrites the table. Do it in a maintenance window, and avoid it on a large live table.",
    example: { unsafe: "ALTER TABLE events SET LOGGED;" },
    related: ["set-access-method"],
  },
  "refresh-matview-non-concurrent": {
    id: "refresh-matview-non-concurrent",
    title: "REFRESH MATERIALIZED VIEW without CONCURRENTLY",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "`REFRESH MATERIALIZED VIEW` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock and blocks all reads.",
    whyUnsafe:
      "`REFRESH MATERIALIZED VIEW` without `CONCURRENTLY` takes an `ACCESS EXCLUSIVE` lock and blocks all reads of the view while it rebuilds.",
    safeRewrite:
      "Use `REFRESH MATERIALIZED VIEW CONCURRENTLY` (requires a unique index on the matview) so reads are not blocked during the rebuild.",
    example: {
      unsafe: "REFRESH MATERIALIZED VIEW mv_sales;",
      safe: "REFRESH MATERIALIZED VIEW CONCURRENTLY mv_sales;",
    },
    related: ["reindex-non-concurrent"],
  },
  "add-exclusion-constraint": {
    id: "add-exclusion-constraint",
    title: "ADD EXCLUDE constraint",
    severity: "error",
    category: "Constraints",
    summary:
      "Adding an `EXCLUDE` constraint builds an index under an `ACCESS EXCLUSIVE` lock, scanning the whole table.",
    whyUnsafe:
      "`ALTER TABLE ... ADD CONSTRAINT ... EXCLUDE` builds an index under an `ACCESS EXCLUSIVE` lock, scanning the whole table.",
    safeRewrite:
      "Adding an exclusion constraint locks the table while it builds the index. Add it during a low-traffic window; on a large table, weigh whether the constraint is necessary.",
    example: {
      unsafe:
        "ALTER TABLE rooms ADD CONSTRAINT no_overlap EXCLUDE USING gist (room WITH =, during WITH &&);",
    },
    related: ["add-unique-constraint"],
  },
  "prefer-jsonb": {
    id: "prefer-jsonb",
    title: "json column type",
    severity: "warning",
    category: "Schema design",
    summary:
      "A `json` column has no equality/ordering operators (`DISTINCT`, `GROUP BY`, `ORDER BY` fail); use `jsonb`.",
    whyUnsafe:
      "A `json` column has no equality or ordering operators, so `SELECT DISTINCT`, `GROUP BY`, `UNION`, and `ORDER BY` on it fail at query time.",
    safeRewrite:
      "Use `jsonb` instead — it supports those operators and indexing. `json` only preserves exact input text and duplicate/key order, which is rarely needed.",
    example: {
      unsafe: "CREATE TABLE events (payload json);",
      safe: "CREATE TABLE events (payload jsonb);",
    },
    related: ["forbidden-column-type"],
  },
  "prefer-bigint-primary-key": {
    id: "prefer-bigint-primary-key",
    title: "Small-integer primary key",
    severity: "warning",
    category: "Schema design",
    summary: "A small-integer primary key overflows its id space; use `bigint`/`bigserial`/`identity`.",
    whyUnsafe:
      "A small-integer `PRIMARY KEY` overflows its id space (`int4` at ~2.1 billion rows, `int2` at ~32 thousand) — a hard outage once ids run out, with no online fix.",
    safeRewrite:
      "Use `bigint`/`bigserial`, or `GENERATED ALWAYS AS IDENTITY`. Migrating a live `int` primary key to `bigint` later is a major, painful operation — start with `bigint`.",
    example: {
      unsafe: "CREATE TABLE events (id serial PRIMARY KEY);",
      safe: "CREATE TABLE events (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY);",
    },
    related: ["add-column-serial", "require-primary-key"],
  },
  "drop-constraint": {
    id: "drop-constraint",
    title: "DROP CONSTRAINT",
    severity: "warning",
    category: "Constraints",
    summary:
      "`DROP CONSTRAINT` removes an integrity guarantee and can break logical-replication replica identity.",
    whyUnsafe:
      "`DROP CONSTRAINT` removes an integrity guarantee (foreign key, check, or unique) that application code may rely on; dropping a primary key or unique constraint can also break logical-replication replica identity.",
    safeRewrite:
      "Confirm no application logic or replication setup depends on the constraint before dropping it.",
    example: { unsafe: "ALTER TABLE orders DROP CONSTRAINT fk_customer;" },
    related: ["add-fk-without-not-valid"],
  },
  "add-trigger": {
    id: "add-trigger",
    title: "CREATE TRIGGER",
    severity: "warning",
    category: "Locking & rewrites",
    summary:
      "`CREATE TRIGGER` takes a `SHARE ROW EXCLUSIVE` lock and changes behavior for every subsequent write.",
    whyUnsafe:
      "`CREATE TRIGGER` takes a `SHARE ROW EXCLUSIVE` lock (blocking writes and other DDL, though not reads) and changes behavior for every subsequent write to the table.",
    safeRewrite:
      "Create the trigger during a low-traffic window; its lock conflicts with concurrent writes on the table.",
    example: {
      unsafe:
        "CREATE TRIGGER trg_audit AFTER INSERT ON users FOR EACH ROW EXECUTE FUNCTION audit();",
    },
  },
  "detach-partition-non-concurrent": {
    id: "detach-partition-non-concurrent",
    title: "DETACH PARTITION without CONCURRENTLY",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "`DETACH PARTITION` takes `ACCESS EXCLUSIVE` on the parent and partition, blocking the whole partitioned table.",
    whyUnsafe:
      "`DETACH PARTITION` takes an `ACCESS EXCLUSIVE` lock on the parent table and the partition, blocking all access to the whole partitioned table while it runs.",
    safeRewrite:
      "Use `ALTER TABLE ... DETACH PARTITION ... CONCURRENTLY` (PostgreSQL 14+; it takes only `SHARE UPDATE EXCLUSIVE` on the parent, so reads and writes keep working). It must run outside a transaction block.",
    example: {
      unsafe: "ALTER TABLE measurement DETACH PARTITION measurement_y2020;",
      safe: "ALTER TABLE measurement DETACH PARTITION measurement_y2020 CONCURRENTLY;",
    },
    related: ["attach-partition"],
  },
  "attach-partition": {
    id: "attach-partition",
    title: "ATTACH PARTITION",
    severity: "warning",
    category: "Locking & rewrites",
    summary:
      "`ATTACH PARTITION` locks and scans the table being attached to validate the partition bound.",
    whyUnsafe:
      "`ATTACH PARTITION` takes an `ACCESS EXCLUSIVE` lock on the table being attached and scans it to validate the partition bound (the parent stays available under `SHARE UPDATE EXCLUSIVE`), so the table being attached is unavailable for the scan's duration.",
    safeRewrite:
      "Add a `CHECK` constraint on the child matching the partition bound and validate it separately first (`ADD CONSTRAINT ... CHECK (...) NOT VALID`, then `VALIDATE CONSTRAINT`); `ATTACH` then skips the scan and the lock is brief.",
    example: {
      unsafe:
        "ALTER TABLE measurement ATTACH PARTITION measurement_y2021 FOR VALUES FROM ('2021-01-01') TO ('2022-01-01');",
      safe: [
        "-- add a matching, validated CHECK first so ATTACH can skip the scan",
        "ALTER TABLE measurement_y2021",
        "  ADD CONSTRAINT measurement_y2021_bound",
        "  CHECK (logdate >= '2021-01-01' AND logdate < '2022-01-01') NOT VALID;",
        "ALTER TABLE measurement_y2021 VALIDATE CONSTRAINT measurement_y2021_bound;",
        "ALTER TABLE measurement ATTACH PARTITION measurement_y2021",
        "  FOR VALUES FROM ('2021-01-01') TO ('2022-01-01');",
      ].join("\n"),
    },
    related: ["detach-partition-non-concurrent"],
  },
  "set-access-method": {
    id: "set-access-method",
    title: "SET ACCESS METHOD",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "`SET ACCESS METHOD` rewrites the entire table and rebuilds its indexes under an `ACCESS EXCLUSIVE` lock.",
    whyUnsafe:
      "`ALTER TABLE ... SET ACCESS METHOD` rewrites the entire table and rebuilds its indexes under an `ACCESS EXCLUSIVE` lock when the access method changes, blocking all reads and writes for the rewrite.",
    safeRewrite:
      "There is no online way to change a table's access method. Do it in a maintenance window, or create a new table with the target access method, copy the data in batches, then swap (expand/contract).",
    example: { unsafe: "ALTER TABLE events SET ACCESS METHOD heap2;" },
    related: ["set-logged-unlogged"],
  },
  "concurrently-in-transaction": {
    id: "concurrently-in-transaction",
    title: "CONCURRENTLY inside a transaction",
    severity: "error",
    category: "Locking & rewrites",
    summary:
      "A `CONCURRENTLY` index operation inside a transaction fails at runtime — Postgres rejects it.",
    whyUnsafe:
      "`CREATE/DROP INDEX CONCURRENTLY` and `REINDEX CONCURRENTLY` cannot run inside a transaction block; this statement runs inside a transaction and will fail at runtime.",
    safeRewrite:
      "Run the `CONCURRENTLY` statement outside the transaction — put it in its own migration, or move it before `BEGIN` / after `COMMIT`. (Note: many migration tools also wrap each migration in an implicit transaction; disable that for this migration.)",
    example: {
      unsafe: "BEGIN;\nCREATE INDEX CONCURRENTLY idx_users_email ON users (email);\nCOMMIT;",
      safe: "CREATE INDEX CONCURRENTLY idx_users_email ON users (email);",
    },
    related: ["add-index-non-concurrent", "enum-value-used-in-transaction"],
  },
  "require-timeout": {
    id: "require-timeout",
    title: "Locking statement without a timeout",
    severity: "warning",
    category: "Locking & rewrites",
    summary: "A blocking-lock statement runs with no `lock_timeout`/`statement_timeout` set.",
    whyUnsafe:
      "This statement takes a lock but no `lock_timeout` is set — if it queues behind a slow query, it blocks every query on the table until it acquires the lock.",
    safeRewrite:
      "Set a bounded `lock_timeout` first, e.g. `SET lock_timeout = '5s';` (or `SET LOCAL` inside a transaction), so the statement fails fast instead of piling up the lock queue. `statement_timeout` also satisfies this.",
    example: {
      unsafe: "ALTER TABLE users ADD COLUMN note text;",
      safe: "SET lock_timeout = '5s';\nALTER TABLE users ADD COLUMN note text;",
    },
    related: ["add-index-non-concurrent"],
  },
  "identifier-too-long": {
    id: "identifier-too-long",
    title: "Identifier longer than 63 bytes",
    severity: "warning",
    category: "Schema design",
    summary:
      "An identifier longer than 63 bytes is silently truncated by PostgreSQL, so two names can collide.",
    whyUnsafe:
      "PostgreSQL truncates identifiers to 63 bytes, so an identifier written longer than that is silently shortened — and two names sharing a 63-byte prefix silently collide.",
    safeRewrite:
      "Shorten the identifier to 63 bytes or fewer so PostgreSQL does not silently truncate it.",
    example: {
      unsafe:
        "ALTER TABLE users ADD COLUMN a_very_long_column_name_that_exceeds_the_postgres_identifier_limit text;",
    },
    related: ["naming-convention"],
  },
  "fk-without-covering-index": {
    id: "fk-without-covering-index",
    title: "Foreign key without a covering index",
    severity: "warning",
    category: "Constraints",
    summary:
      "A foreign key with no covering index makes every parent change scan and lock the child.",
    whyUnsafe:
      "A foreign key whose referencing column has no covering index makes referential checks and `ON DELETE/UPDATE` actions on the parent scan and lock the child on every change.",
    safeRewrite:
      "Add a covering index on the referencing column, e.g. `CREATE INDEX CONCURRENTLY ON orders (customer_id);`.",
    example: {
      unsafe: "ALTER TABLE orders ADD COLUMN customer_id bigint REFERENCES customers (id);",
      safe: "ALTER TABLE orders ADD COLUMN customer_id bigint REFERENCES customers (id);\nCREATE INDEX CONCURRENTLY ON orders (customer_id);",
    },
    related: ["add-fk-without-not-valid"],
  },
  "enum-value-used-in-transaction": {
    id: "enum-value-used-in-transaction",
    title: "New enum value used in the same transaction",
    severity: "warning",
    category: "Locking & rewrites",
    summary:
      "`ALTER TYPE … ADD VALUE` then using that value in the same transaction fails at runtime.",
    whyUnsafe:
      "This `ALTER TYPE ... ADD VALUE` adds an enum value that is used later in the same transaction. PostgreSQL forbids using a newly added enum value in the transaction that added it; the later statement fails at runtime with \"`unsafe use of new value`\".",
    safeRewrite:
      "Add the enum value in its own migration (or before `BEGIN` / outside the wrapping transaction) so it is committed before any statement uses it. Many migration tools wrap each migration in an implicit transaction — disable that for this migration if you must add and use the value together.",
    example: {
      unsafe:
        "BEGIN;\nALTER TYPE mood ADD VALUE 'happy';\nUPDATE surveys SET mood = 'happy';\nCOMMIT;",
    },
    related: ["concurrently-in-transaction"],
  },
  "require-primary-key": {
    id: "require-primary-key",
    title: "Table without a primary key",
    severity: "warning",
    category: "Policy",
    summary: "(opt-in) A `CREATE TABLE` the migration leaves without a primary key.",
    whyUnsafe:
      "This table is created without a primary key. Logical replication needs one (a table with no replica identity rejects `UPDATE/DELETE`), and many ORMs and tools assume every table has one.",
    safeRewrite:
      "Add a primary key — inline (`PRIMARY KEY` on a column, or a table-level `PRIMARY KEY (...)`) or in a later `ALTER TABLE ... ADD PRIMARY KEY` in the same migration.",
    example: {
      unsafe: "CREATE TABLE events (id int, payload jsonb);",
      safe: "CREATE TABLE events (id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY, payload jsonb);",
    },
    related: ["add-primary-key-without-index", "require-not-null"],
  },
  "require-not-null": {
    id: "require-not-null",
    title: "Nullable column (policy)",
    severity: "warning",
    category: "Policy",
    summary: "(opt-in) A `CREATE TABLE` with a column left nullable.",
    whyUnsafe:
      "A column has no `NOT NULL` constraint; this policy requires every column in a `CREATE TABLE` to be `NOT NULL`.",
    safeRewrite:
      "Add `NOT NULL` to the column (it is free on a new, empty table), or add it later in the same migration with `ALTER TABLE ... ALTER COLUMN ... SET NOT NULL`. For an intentionally nullable column, suppress with `-- pgsafe:ignore require-not-null ...`.",
    example: {
      unsafe: "CREATE TABLE users (id bigint PRIMARY KEY, email text);",
      safe: "CREATE TABLE users (id bigint PRIMARY KEY, email text NOT NULL);",
    },
    related: ["require-primary-key", "forbid-nullable-fk"],
  },
  "naming-convention": {
    id: "naming-convention",
    title: "Naming convention violation",
    severity: "warning",
    category: "Policy",
    summary: "(opt-in) An introduced name that doesn't match the configured regex for its kind.",
    whyUnsafe:
      "An introduced name (table/column/index/constraint/sequence/trigger/schema) does not match the configured naming pattern for its kind.",
    safeRewrite:
      "Rename the object to match the convention, or adjust the `[naming]` pattern in your config.",
    example: {
      unsafe: 'CREATE TABLE "UserAccounts" (id bigint PRIMARY KEY);',
      safe: "CREATE TABLE user_accounts (id bigint PRIMARY KEY);",
    },
    related: ["identifier-too-long"],
  },
  "forbidden-column-type": {
    id: "forbidden-column-type",
    title: "Forbidden column type",
    severity: "warning",
    category: "Policy",
    summary: "(opt-in) A column whose type is in the configured forbidden set.",
    whyUnsafe:
      "A column uses a type your policy disallows (e.g. `timestamp` banned in favor of `timestamptz`).",
    safeRewrite:
      "Change the column to an allowed type, or remove this type from the `[forbidden-types]` section of your config.",
    example: {
      unsafe: "CREATE TABLE events (occurred_at timestamp);",
      safe: "CREATE TABLE events (occurred_at timestamptz);",
    },
    related: ["prefer-jsonb"],
  },
  "require-if-exists": {
    id: "require-if-exists",
    title: "Missing IF [NOT] EXISTS",
    severity: "warning",
    category: "Policy",
    summary: "(opt-in) A `CREATE/DROP` without `IF NOT EXISTS` / `IF EXISTS`.",
    whyUnsafe:
      "`CREATE TABLE` without `IF NOT EXISTS` is not idempotent — it errors if the table already exists.",
    safeRewrite:
      "Add `IF NOT EXISTS` (`CREATE`) or `IF EXISTS` (`DROP`) so re-running the migration does not error.",
    example: {
      unsafe: "CREATE TABLE users (id bigint PRIMARY KEY);",
      safe: "CREATE TABLE IF NOT EXISTS users (id bigint PRIMARY KEY);",
    },
  },
  "unchecked-do-block": {
    id: "unchecked-do-block",
    title: "Unchecked DO block",
    severity: "warning",
    category: "Policy",
    summary: "(opt-in) A `DO` block containing SQL pgsafe can't statically analyze.",
    whyUnsafe:
      "This `DO` block contains dynamic SQL (`EXECUTE`) or a body pgsafe could not parse; the rest of the block was checked, but that part was not.",
    safeRewrite:
      "Move the dynamic DDL/DML out of the `DO` block into top-level statements so pgsafe can check it, or suppress this finding with an inline `-- pgsafe:ignore unchecked-do-block <reason>` after review.",
    example: {
      unsafe: "DO $$ BEGIN EXECUTE 'ALTER TABLE users ADD COLUMN x int'; END $$;",
    },
  },
  "require-comment": {
    id: "require-comment",
    title: "Missing COMMENT",
    severity: "warning",
    category: "Policy",
    summary: "(opt-in) A new table or column left without a `COMMENT`.",
    whyUnsafe: "A new table or column has no `COMMENT`.",
    safeRewrite:
      "Add a `COMMENT ON TABLE` / `COMMENT ON COLUMN` in the migration documenting the new object.",
    example: {
      unsafe: "CREATE TABLE users (id bigint PRIMARY KEY);",
      safe: "CREATE TABLE users (id bigint PRIMARY KEY);\nCOMMENT ON TABLE users IS 'Application user accounts.';",
    },
  },
  "require-columns": {
    id: "require-columns",
    title: "Missing required column",
    severity: "warning",
    category: "Policy",
    summary: "(opt-in) A `CREATE TABLE` missing a configured required column (e.g. `created_at`).",
    whyUnsafe:
      "A `CREATE TABLE` is missing a column the policy requires (e.g. `created_at`). A later `ALTER TABLE … ADD COLUMN` in the same migration also satisfies it.",
    safeRewrite:
      "Add the column to the `CREATE TABLE` (or a later `ALTER TABLE … ADD COLUMN` in the same migration), or remove it from `required-columns` in your config.",
    example: {
      unsafe: "CREATE TABLE orders (id bigint PRIMARY KEY);",
      safe: "CREATE TABLE orders (id bigint PRIMARY KEY, created_at timestamptz NOT NULL);",
    },
    related: ["require-not-null"],
  },
  "forbid-nullable-fk": {
    id: "forbid-nullable-fk",
    title: "Nullable foreign key",
    severity: "warning",
    category: "Policy",
    summary: "(opt-in) A nullable foreign-key column in a `CREATE TABLE`.",
    whyUnsafe:
      "A foreign-key column is nullable; a nullable foreign key allows orphan rows and unexpected join results.",
    safeRewrite:
      "Add `NOT NULL` to the foreign-key column (inline or a later `SET NOT NULL`), or suppress if a nullable foreign key is intended.",
    example: {
      unsafe:
        "CREATE TABLE orders (id bigint PRIMARY KEY, customer_id bigint REFERENCES customers (id));",
      safe: "CREATE TABLE orders (id bigint PRIMARY KEY, customer_id bigint NOT NULL REFERENCES customers (id));",
    },
    related: ["require-not-null", "fk-without-covering-index"],
  },
};

// Build-time drift-guard: the prose catalog must cover exactly the crate's rule set.
const proseIds = new Set(Object.keys(RULES));
const crateIds = new Set(catalog.rules as string[]);
const missing = [...crateIds].filter((id) => !proseIds.has(id));
const extra = [...proseIds].filter((id) => !crateIds.has(id));
if (missing.length || extra.length) {
  throw new Error(
    `rules.ts out of sync with the crate catalog. Missing prose for: [${missing.join(", ")}]. ` +
      `Unknown ids (not in crate): [${extra.join(", ")}]. ` +
      `Regenerate rules.catalog.json (pgsafe --list-rules --format json) and update RULES.`,
  );
}

export const RULE_LIST: RuleDoc[] = (catalog.rules as string[]).map((id) => RULES[id]);
