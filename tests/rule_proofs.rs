//! Rule-proving harness: runs each flagged DDL against a real Postgres and asserts the
//! observed lock mode and table-rewrite behavior match the rule's claim. The DB-backed
//! proof is `#[ignore]`d (run it with `--ignored` and a `DATABASE_URL` pointing at a
//! throwaway Postgres); the helpers below are pure and run in the normal test suite.

/// Rank Postgres relation lock modes from weakest (1) to strongest (8), so the harness can
/// reduce the set of modes a backend holds on a table to the single strongest one. Order
/// follows the Postgres docs' "Table-Level Locks". Unknown modes rank 0.
fn lock_strength(mode: &str) -> u8 {
    match mode {
        "AccessShareLock" => 1,
        "RowShareLock" => 2,
        "RowExclusiveLock" => 3,
        "ShareUpdateExclusiveLock" => 4,
        "ShareLock" => 5,
        "ShareRowExclusiveLock" => 6,
        "ExclusiveLock" => 7,
        "AccessExclusiveLock" => 8,
        _ => 0,
    }
}

/// The major version from `server_version_num` (PG10+: `MMmmpp`, e.g. 160003 -> 16).
fn server_major(version_num: i32) -> u32 {
    // try_from avoids a sign-losing `as` cast; server_version_num is always non-negative,
    // so the impossible negative case falls back to 0.
    u32::try_from(version_num / 10_000).unwrap_or(0)
}

/// The effect of a DDL on the watched relation's storage, derived from its `relfilenode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RewriteOutcome {
    /// Same relfilenode — no table rewrite (e.g. a metadata-only change).
    Unchanged,
    /// New relfilenode — the relation was rewritten/rebuilt.
    Changed,
    /// The relation no longer exists (e.g. DROP TABLE).
    Gone,
}

/// Classify the rewrite from the watched relation's relfilenode before the DDL and after it
/// (`after` is `None` when the relation no longer exists).
fn classify_rewrite(before: u32, after: Option<u32>) -> RewriteOutcome {
    match after {
        None => RewriteOutcome::Gone,
        Some(a) if a != before => RewriteOutcome::Changed,
        Some(_) => RewriteOutcome::Unchanged,
    }
}

#[test]
fn lock_strength_orders_modes() {
    assert!(lock_strength("AccessExclusiveLock") > lock_strength("ShareLock"));
    assert!(lock_strength("ShareLock") > lock_strength("AccessShareLock"));
    assert_eq!(lock_strength("not-a-real-mode"), 0);
}

#[test]
fn server_major_extracts_major_version() {
    assert_eq!(server_major(160003), 16);
    assert_eq!(server_major(140012), 14);
    assert_eq!(server_major(180000), 18);
}

#[test]
fn classify_rewrite_distinguishes_outcomes() {
    assert_eq!(classify_rewrite(100, Some(100)), RewriteOutcome::Unchanged);
    assert_eq!(classify_rewrite(100, Some(200)), RewriteOutcome::Changed);
    assert_eq!(classify_rewrite(100, None), RewriteOutcome::Gone);
}

use std::ops::RangeInclusive;

use postgres::{Client, NoTls};

/// One empirical proof. `setup` creates and seeds the objects (committed); `table` is the object(s)
/// dropped for cleanup — a comma-separated list when a case owns more than one (e.g. an FK's parent
/// and child) — with `DROP TABLE IF EXISTS … CASCADE` removing dependents; `watch` is the relation
/// whose lock + relfilenode are observed (often `table`, but e.g. the index for REINDEX or the matview
/// for REFRESH); `also_watch` asserts a lock on a second relation. `pg` is the inclusive major-version
/// range the case applies to.
struct ProofCase {
    rule: &'static str,
    table: &'static str,
    watch: &'static str,
    setup: &'static str,
    ddl: &'static str,
    expect_lock: &'static str,
    expect_rewrite: RewriteOutcome,
    /// A second relation to also assert a lock on (e.g. the parent table an FK locks), as
    /// (relation, expected strongest lock). `None` for single-relation cases.
    also_watch: Option<(&'static str, &'static str)>,
    pg: RangeInclusive<u32>,
}

/// The proof cases. The 4th entry (`proof_control`) is a *control*: a strong-lock statement that
/// does NOT rewrite, proving the rewrite detector discriminates (it must observe
/// `rewrite = Unchanged`, not `Changed`).
fn cases() -> Vec<ProofCase> {
    vec![
        ProofCase {
            rule: "add-index-non-concurrent",
            table: "proof_add_index",
            watch: "proof_add_index",
            setup: "CREATE TABLE proof_add_index (c int); \
                    INSERT INTO proof_add_index SELECT g FROM generate_series(1, 3) g;",
            ddl: "CREATE INDEX proof_add_index_ix ON proof_add_index (c)",
            expect_lock: "ShareLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "alter-column-type",
            table: "proof_alter_type",
            watch: "proof_alter_type",
            setup: "CREATE TABLE proof_alter_type (c int); \
                    INSERT INTO proof_alter_type SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_alter_type ALTER COLUMN c TYPE bigint",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Changed,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "add-column-volatile-default",
            table: "proof_vol_default",
            watch: "proof_vol_default",
            setup: "CREATE TABLE proof_vol_default (id int); \
                    INSERT INTO proof_vol_default SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_vol_default ADD COLUMN u uuid DEFAULT gen_random_uuid()",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Changed,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "(control: strong lock, no rewrite)",
            table: "proof_control",
            watch: "proof_control",
            setup: "CREATE TABLE proof_control (id int); \
                    INSERT INTO proof_control SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_control ADD COLUMN c int",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "reindex-non-concurrent",
            table: "proof_reindex",
            watch: "proof_reindex_ix",
            setup: "CREATE TABLE proof_reindex (c int); \
                    INSERT INTO proof_reindex SELECT g FROM generate_series(1, 3) g; \
                    CREATE INDEX proof_reindex_ix ON proof_reindex (c);",
            ddl: "REINDEX TABLE proof_reindex",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Changed,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "drop-index-non-concurrent",
            table: "proof_drop_index",
            watch: "proof_drop_index",
            setup: "CREATE TABLE proof_drop_index (c int); \
                    INSERT INTO proof_drop_index SELECT g FROM generate_series(1, 3) g; \
                    CREATE INDEX proof_drop_index_ix ON proof_drop_index (c);",
            ddl: "DROP INDEX proof_drop_index_ix",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "add-unique-constraint",
            table: "proof_add_unique",
            watch: "proof_add_unique",
            setup: "CREATE TABLE proof_add_unique (c int); \
                    INSERT INTO proof_add_unique SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_add_unique ADD CONSTRAINT u UNIQUE (c)",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "add-primary-key-without-index",
            table: "proof_add_pk",
            watch: "proof_add_pk",
            setup: "CREATE TABLE proof_add_pk (c int NOT NULL); \
                    INSERT INTO proof_add_pk SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_add_pk ADD PRIMARY KEY (c)",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "set-not-null",
            table: "proof_set_not_null",
            watch: "proof_set_not_null",
            setup: "CREATE TABLE proof_set_not_null (c int); \
                    INSERT INTO proof_set_not_null SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_set_not_null ALTER COLUMN c SET NOT NULL",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "add-check-without-not-valid",
            table: "proof_add_check",
            watch: "proof_add_check",
            setup: "CREATE TABLE proof_add_check (c int); \
                    INSERT INTO proof_add_check SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_add_check ADD CONSTRAINT ck CHECK (c > 0)",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "add-exclusion-constraint",
            table: "proof_add_exclude",
            watch: "proof_add_exclude",
            setup: "CREATE TABLE proof_add_exclude (r int4range);",
            ddl: "ALTER TABLE proof_add_exclude ADD CONSTRAINT ex EXCLUDE USING gist (r WITH &&)",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "set-logged-unlogged",
            table: "proof_set_logged",
            watch: "proof_set_logged",
            setup: "CREATE UNLOGGED TABLE proof_set_logged (c int); \
                    INSERT INTO proof_set_logged SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_set_logged SET LOGGED",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Changed,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "refresh-matview-non-concurrent",
            table: "proof_mv_base",
            watch: "proof_mv",
            setup: "CREATE TABLE proof_mv_base (c int); \
                    INSERT INTO proof_mv_base SELECT g FROM generate_series(1, 3) g; \
                    CREATE MATERIALIZED VIEW proof_mv AS SELECT * FROM proof_mv_base;",
            ddl: "REFRESH MATERIALIZED VIEW proof_mv",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Changed,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "add-column-serial",
            table: "proof_add_serial",
            watch: "proof_add_serial",
            setup: "CREATE TABLE proof_add_serial (id int); \
                    INSERT INTO proof_add_serial SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_add_serial ADD COLUMN s serial",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Changed,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "add-column-identity",
            table: "proof_add_identity",
            watch: "proof_add_identity",
            setup: "CREATE TABLE proof_add_identity (id int); \
                    INSERT INTO proof_add_identity SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_add_identity ADD COLUMN s int GENERATED ALWAYS AS IDENTITY",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Changed,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "add-column-generated-stored",
            table: "proof_add_generated",
            watch: "proof_add_generated",
            setup: "CREATE TABLE proof_add_generated (id int); \
                    INSERT INTO proof_add_generated SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_add_generated ADD COLUMN g int GENERATED ALWAYS AS (id * 2) STORED",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Changed,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "truncate",
            table: "proof_truncate",
            watch: "proof_truncate",
            setup: "CREATE TABLE proof_truncate (c int); \
                    INSERT INTO proof_truncate SELECT g FROM generate_series(1, 3) g;",
            ddl: "TRUNCATE proof_truncate",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Changed,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "drop-table",
            table: "proof_drop_table",
            watch: "proof_drop_table",
            setup: "CREATE TABLE proof_drop_table (c int); \
                    INSERT INTO proof_drop_table SELECT g FROM generate_series(1, 3) g;",
            ddl: "DROP TABLE proof_drop_table",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Gone,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "vacuum-full-cluster",
            table: "proof_cluster",
            watch: "proof_cluster",
            setup: "CREATE TABLE proof_cluster (c int); \
                    INSERT INTO proof_cluster SELECT g FROM generate_series(1, 3) g; \
                    CREATE INDEX proof_cluster_ix ON proof_cluster (c);",
            ddl: "CLUSTER proof_cluster USING proof_cluster_ix",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Changed,
            also_watch: None,
            pg: 14..=18,
        },
        ProofCase {
            rule: "add-fk-without-not-valid",
            table: "proof_fk_parent, proof_fk_child",
            watch: "proof_fk_child",
            setup: "CREATE TABLE proof_fk_parent (id int PRIMARY KEY); \
                    INSERT INTO proof_fk_parent SELECT g FROM generate_series(1, 3) g; \
                    CREATE TABLE proof_fk_child (pid int); \
                    INSERT INTO proof_fk_child SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_fk_child ADD CONSTRAINT fk \
                  FOREIGN KEY (pid) REFERENCES proof_fk_parent (id)",
            expect_lock: "ShareRowExclusiveLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: Some(("proof_fk_parent", "ShareRowExclusiveLock")),
            pg: 14..=18,
        },
        ProofCase {
            rule: "detach-partition-non-concurrent",
            table: "proof_detach",
            watch: "proof_detach",
            setup: "CREATE TABLE proof_detach (id int) PARTITION BY RANGE (id); \
                    CREATE TABLE proof_detach_p1 PARTITION OF proof_detach FOR VALUES FROM (0) TO (100); \
                    INSERT INTO proof_detach_p1 SELECT g FROM generate_series(0, 99) g;",
            ddl: "ALTER TABLE proof_detach DETACH PARTITION proof_detach_p1",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: Some(("proof_detach_p1", "AccessExclusiveLock")),
            pg: 14..=18,
        },
        ProofCase {
            rule: "attach-partition",
            table: "proof_attach, proof_attach_child",
            watch: "proof_attach_child",
            setup: "CREATE TABLE proof_attach (id int) PARTITION BY RANGE (id); \
                    CREATE TABLE proof_attach_child (id int); \
                    INSERT INTO proof_attach_child SELECT g FROM generate_series(100, 199) g;",
            ddl: "ALTER TABLE proof_attach ATTACH PARTITION proof_attach_child \
                  FOR VALUES FROM (100) TO (200)",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: RewriteOutcome::Unchanged,
            also_watch: Some(("proof_attach", "ShareUpdateExclusiveLock")),
            pg: 14..=18,
        },
    ]
}

/// What the harness observed for one case.
struct Observed {
    lock: String,
    rewrite: RewriteOutcome,
    also_lock: Option<String>,
}

/// Connect to `DATABASE_URL` (NoTls — throwaway local/CI Postgres only).
fn connect() -> Client {
    let url = std::env::var("DATABASE_URL")
        .expect("set DATABASE_URL to a throwaway Postgres to run the rule proofs");
    Client::connect(&url, NoTls).expect("connect to DATABASE_URL")
}

/// The current relfilenode of the relation with the given oid, or `None` if it no longer exists.
fn relfilenode(c: &mut Client, oid: u32) -> Option<u32> {
    c.query_opt("SELECT relfilenode FROM pg_class WHERE oid = $1", &[&oid])
        .expect("read relfilenode")
        .map(|row| row.get::<_, u32>(0))
}

/// Read the strongest relation lock `pid` holds on `oid` from the observer session.
fn observe_lock(observer: &mut Client, pid: i32, oid: u32, what: &str) -> String {
    observer
        .query(
            "SELECT mode FROM pg_locks \
             WHERE pid = $1 AND locktype = 'relation' AND relation = $2 AND granted",
            &[&pid, &oid],
        )
        .expect("read pg_locks")
        .iter()
        .map(|r| r.get::<_, String>(0))
        .max_by_key(|m| lock_strength(m))
        .unwrap_or_else(|| panic!("no relation lock observed on {what} for backend {pid}"))
}

/// Run one proof case: seed (committed), run the DDL in an open transaction, read the held
/// lock from the observer session and the rewrite from the actor session, then roll back and
/// drop the throwaway table.
fn run_case(actor: &mut Client, observer: &mut Client, case: &ProofCase) -> Observed {
    let root = case.table;
    actor
        .batch_execute(&format!("DROP TABLE IF EXISTS {root} CASCADE"))
        .expect("drop pre-existing");
    actor.batch_execute(case.setup).expect("setup");

    // Resolve the watched relation's oid BEFORE the transaction so it survives a drop.
    let oid: u32 = actor
        .query_one(&format!("SELECT '{}'::regclass::oid", case.watch), &[])
        .expect("resolve watch oid")
        .get::<_, u32>(0);
    let also_oid: Option<u32> = case.also_watch.map(|(rel, _)| {
        actor
            .query_one(&format!("SELECT '{rel}'::regclass::oid"), &[])
            .expect("resolve also_watch oid")
            .get::<_, u32>(0)
    });
    let pid: i32 = actor
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("backend pid")
        .get::<_, i32>(0);
    let rel_before = relfilenode(actor, oid).expect("watched relation exists before the ddl");

    // Act: run the flagged DDL in an OPEN transaction so the locks stay held.
    actor.batch_execute("BEGIN").expect("begin");
    actor.batch_execute(case.ddl).expect("run flagged ddl");
    let rewrite = classify_rewrite(rel_before, relfilenode(actor, oid));

    let lock = observe_lock(observer, pid, oid, case.watch);
    let also_lock = case
        .also_watch
        .zip(also_oid)
        .map(|((rel, _), o)| observe_lock(observer, pid, o, rel));

    actor.batch_execute("ROLLBACK").expect("rollback");
    actor
        .batch_execute(&format!("DROP TABLE IF EXISTS {root} CASCADE"))
        .expect("drop");

    Observed {
        lock,
        rewrite,
        also_lock,
    }
}

#[test]
#[ignore = "requires DATABASE_URL pointing at a throwaway Postgres (run with --ignored)"]
fn rules_hold_against_real_postgres() {
    let mut actor = connect();
    let mut observer = connect();
    let major = server_major(
        actor
            .query_one("SELECT current_setting('server_version_num')::int", &[])
            .expect("read server_version_num")
            .get::<_, i32>(0),
    );

    let mut ran = 0;
    let mut failures = Vec::new();
    println!("\n=== pgsafe rule proofs (PostgreSQL {major}) ===");
    for case in cases() {
        if !case.pg.contains(&major) {
            println!("  SKIP {:<34} (out of pg range)", case.rule);
            continue;
        }
        ran += 1;
        let obs = run_case(&mut actor, &mut observer, &case);
        let lock_ok = obs.lock == case.expect_lock;
        let rewrite_ok = obs.rewrite == case.expect_rewrite;
        let also_ok = match case.also_watch {
            Some((_, expected)) => obs.also_lock.as_deref() == Some(expected),
            None => true,
        };
        let also_note = match (&case.also_watch, &obs.also_lock) {
            (Some((rel, _)), Some(l)) => format!(" also[{rel}]={l}"),
            _ => String::new(),
        };
        println!(
            "  {} {:<34} lock={} rewrite={:?}{}",
            if lock_ok && rewrite_ok && also_ok {
                "OK  "
            } else {
                "FAIL"
            },
            case.rule,
            obs.lock,
            obs.rewrite,
            also_note,
        );
        if !lock_ok {
            failures.push(format!(
                "{}: lock expected {}, observed {}",
                case.rule, case.expect_lock, obs.lock
            ));
        }
        if !rewrite_ok {
            failures.push(format!(
                "{}: rewrite expected {:?}, observed {:?}",
                case.rule, case.expect_rewrite, obs.rewrite
            ));
        }
        if !also_ok {
            failures.push(format!(
                "{}: also_watch lock expected {:?}, observed {:?}",
                case.rule,
                case.also_watch.map(|(_, e)| e),
                obs.also_lock
            ));
        }
    }
    assert!(ran > 0, "no proof cases applied to PostgreSQL {major}");
    assert!(
        failures.is_empty(),
        "rule proofs failed on PostgreSQL {major}:\n{}",
        failures.join("\n")
    );
}

/// A statement the linter flags because it fails outright on a populated table.
struct FailureCase {
    rule: &'static str,
    table: &'static str,
    setup: &'static str,
    ddl: &'static str,
    sqlstate: &'static str,
    pg: RangeInclusive<u32>,
}

fn failure_cases() -> Vec<FailureCase> {
    vec![
        FailureCase {
            rule: "add-column-not-null-no-default",
            table: "proof_nn_fail",
            setup: "CREATE TABLE proof_nn_fail (id int); \
                    INSERT INTO proof_nn_fail SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_nn_fail ADD COLUMN x int NOT NULL",
            sqlstate: "23502",
            pg: 14..=18,
        },
        FailureCase {
            rule: "enum-value-used-in-transaction",
            table: "proof_enum",
            setup: "DROP TYPE IF EXISTS proof_enum_t CASCADE; \
                    CREATE TYPE proof_enum_t AS ENUM ('a'); \
                    CREATE TABLE proof_enum (m proof_enum_t);",
            ddl: "BEGIN; ALTER TYPE proof_enum_t ADD VALUE 'b'; \
                  INSERT INTO proof_enum VALUES ('b'); COMMIT;",
            sqlstate: "55P04",
            pg: 14..=18,
        },
        // A column-level PRIMARY KEY implies NOT NULL, so adding one to a populated table fails the
        // same way as an explicit NOT NULL with no default (backs `add-column-not-null-no-default`
        // treating ConstrPrimary as NOT NULL).
        FailureCase {
            rule: "add-column-not-null-no-default (PRIMARY KEY)",
            table: "proof_pk_fail",
            setup: "CREATE TABLE proof_pk_fail (id int); \
                    INSERT INTO proof_pk_fail SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_pk_fail ADD COLUMN c int PRIMARY KEY",
            sqlstate: "23502",
            pg: 14..=18,
        },
        // An inline CHECK on an ADD COLUMN with a DEFAULT is validated against the (defaulted) existing
        // rows — a default that violates the check errors, proving the validation happens (not free).
        FailureCase {
            rule: "add-check-without-not-valid (inline on ADD COLUMN with DEFAULT)",
            table: "proof_inline_check",
            setup: "CREATE TABLE proof_inline_check (id int); \
                    INSERT INTO proof_inline_check SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_inline_check ADD COLUMN x int DEFAULT 1 CHECK (x > 5)",
            sqlstate: "23514",
            pg: 14..=18,
        },
        // An inline FK on an ADD COLUMN with a DEFAULT is validated against the (defaulted) existing
        // rows — a default with no matching parent row errors, proving the FK validation happens.
        FailureCase {
            rule: "add-fk-without-not-valid (inline on ADD COLUMN with DEFAULT)",
            table: "proof_inline_fk",
            setup: "DROP TABLE IF EXISTS proof_inline_fk_parent CASCADE; \
                    CREATE TABLE proof_inline_fk_parent (id int PRIMARY KEY); \
                    CREATE TABLE proof_inline_fk (id int); \
                    INSERT INTO proof_inline_fk SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_inline_fk ADD COLUMN pid int DEFAULT 999 \
                  REFERENCES proof_inline_fk_parent(id)",
            sqlstate: "23503",
            pg: 14..=18,
        },
        // REJECTION proof: `REFRESH MATERIALIZED VIEW CONCURRENTLY` IS allowed inside a transaction
        // (unlike CREATE/DROP INDEX CONCURRENTLY), so `concurrently-in-transaction` must NOT flag it.
        // `sqlstate: "(succeeded)"` asserts the statement runs to completion in a transaction.
        FailureCase {
            rule: "refresh-matview-concurrently-allowed-in-txn (must NOT be flagged)",
            table: "proof_refresh_base",
            setup: "CREATE TABLE proof_refresh_base (id int); \
                    INSERT INTO proof_refresh_base SELECT g FROM generate_series(1, 3) g; \
                    CREATE MATERIALIZED VIEW proof_refresh_mv AS SELECT id FROM proof_refresh_base; \
                    CREATE UNIQUE INDEX proof_refresh_mv_uidx ON proof_refresh_mv (id);",
            ddl: "BEGIN; REFRESH MATERIALIZED VIEW CONCURRENTLY proof_refresh_mv; COMMIT;",
            sqlstate: "(succeeded)",
            pg: 14..=18,
        },
    ]
}

#[test]
#[ignore = "requires DATABASE_URL pointing at a throwaway Postgres (run with --ignored)"]
fn statements_fail_as_claimed() {
    let mut client = connect();
    let major = server_major(
        client
            .query_one("SELECT current_setting('server_version_num')::int", &[])
            .expect("read server_version_num")
            .get::<_, i32>(0),
    );

    let mut ran = 0;
    let mut failures = Vec::new();
    println!("\n=== pgsafe failure proofs (PostgreSQL {major}) ===");
    for case in failure_cases() {
        if !case.pg.contains(&major) {
            println!("  SKIP {:<34} (out of pg range)", case.rule);
            continue;
        }
        ran += 1;
        let t = case.table;
        client
            .batch_execute(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
            .expect("drop pre-existing");
        client.batch_execute(case.setup).expect("setup");

        // Run the flagged DDL in autocommit; it must fail with the claimed SQLSTATE.
        let got = match client.batch_execute(case.ddl) {
            Ok(()) => "(succeeded)".to_string(),
            Err(e) => e
                .code()
                .map(|c| c.code().to_string())
                .unwrap_or_else(|| "(no sqlstate)".to_string()),
        };
        // A failing ddl that opened an explicit transaction leaves it aborted; roll back so the
        // cleanup DROP below can run. Harmless no-op when there is no open transaction.
        client.batch_execute("ROLLBACK").ok();
        let ok = got == case.sqlstate;
        println!(
            "  {} {:<34} sqlstate={}",
            if ok { "OK  " } else { "FAIL" },
            case.rule,
            got
        );
        if !ok {
            failures.push(format!(
                "{}: expected failure SQLSTATE {}, got {}",
                case.rule, case.sqlstate, got
            ));
        }

        client
            .batch_execute(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
            .expect("drop");
    }
    assert!(ran > 0, "no failure cases applied to PostgreSQL {major}");
    assert!(
        failures.is_empty(),
        "failure proofs failed on PostgreSQL {major}:\n{}",
        failures.join("\n")
    );
}

/// A statement whose hazard is (or isn't) that it sequentially scans the table to validate a
/// constraint — proven deterministically by whether `pg_stat_user_tables.seq_scan` increments. A
/// plain metadata-only change does not scan; an inline CHECK on `ADD COLUMN` does (even with no
/// default — NULL rows are still scanned); an inline FK on `ADD COLUMN` with no default does NOT
/// (NULL is FK-exempt, so Postgres skips the scan). PG15+ only (uses `pg_stat_force_next_flush`
/// for a deterministic stats read).
struct ScanCase {
    rule: &'static str,
    table: &'static str,
    setup: &'static str,
    ddl: &'static str,
    expect_scan: bool,
    pg: RangeInclusive<u32>,
}

fn scan_cases() -> Vec<ScanCase> {
    vec![
        ScanCase {
            rule: "plain ADD COLUMN (baseline — no scan)",
            table: "proof_scan_plain",
            setup: "CREATE TABLE proof_scan_plain (id int); \
                    INSERT INTO proof_scan_plain SELECT g FROM generate_series(1, 1000) g;",
            ddl: "ALTER TABLE proof_scan_plain ADD COLUMN x int",
            expect_scan: false,
            pg: 15..=18,
        },
        ScanCase {
            rule: "add-check-without-not-valid (inline CHECK on ADD COLUMN, no default — scans)",
            table: "proof_scan_check_nd",
            setup: "CREATE TABLE proof_scan_check_nd (id int); \
                    INSERT INTO proof_scan_check_nd SELECT g FROM generate_series(1, 1000) g;",
            ddl: "ALTER TABLE proof_scan_check_nd ADD COLUMN x int CHECK (x > 5)",
            expect_scan: true,
            pg: 15..=18,
        },
        ScanCase {
            rule: "add-fk-without-not-valid (inline FK on ADD COLUMN, no default — does NOT scan)",
            table: "proof_scan_fk_nd",
            setup: "DROP TABLE IF EXISTS proof_scan_fk_parent CASCADE; \
                    CREATE TABLE proof_scan_fk_parent (id int PRIMARY KEY); \
                    CREATE TABLE proof_scan_fk_nd (id int); \
                    INSERT INTO proof_scan_fk_nd SELECT g FROM generate_series(1, 1000) g;",
            ddl: "ALTER TABLE proof_scan_fk_nd ADD COLUMN pid int \
                  REFERENCES proof_scan_fk_parent(id)",
            expect_scan: false,
            pg: 15..=18,
        },
    ]
}

/// `pg_stat_user_tables.seq_scan` for `table`, after forcing the stats collector to flush so the
/// read is deterministic (PG15+).
fn seq_scan_count(c: &mut Client, table: &str) -> i64 {
    c.batch_execute("SELECT pg_stat_force_next_flush()")
        .expect("force stats flush");
    c.query_one(
        "SELECT coalesce(seq_scan, 0) FROM pg_stat_user_tables WHERE relname = $1",
        &[&table],
    )
    .expect("read seq_scan")
    .get::<_, i64>(0)
}

#[test]
#[ignore = "requires DATABASE_URL pointing at a throwaway Postgres (run with --ignored)"]
fn statements_scan_as_claimed() {
    let mut client = connect();
    let major = server_major(
        client
            .query_one("SELECT current_setting('server_version_num')::int", &[])
            .expect("read server_version_num")
            .get::<_, i32>(0),
    );

    let mut ran = 0;
    let mut failures = Vec::new();
    println!("\n=== pgsafe scan proofs (PostgreSQL {major}) ===");
    for case in scan_cases() {
        if !case.pg.contains(&major) {
            println!("  SKIP {:<34} (out of pg range)", case.rule);
            continue;
        }
        ran += 1;
        let t = case.table;
        client
            .batch_execute(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
            .expect("drop pre-existing");
        client.batch_execute(case.setup).expect("setup");

        let before = seq_scan_count(&mut client, t);
        client.batch_execute(case.ddl).expect("run ddl");
        let after = seq_scan_count(&mut client, t);
        let scanned = after > before;

        let ok = scanned == case.expect_scan;
        println!(
            "  {} {:<60} scanned={} (seq_scan {} -> {})",
            if ok { "OK  " } else { "FAIL" },
            case.rule,
            scanned,
            before,
            after
        );
        if !ok {
            failures.push(format!(
                "{}: expected scan={}, observed scan={}",
                case.rule, case.expect_scan, scanned
            ));
        }

        client
            .batch_execute(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
            .expect("drop");
    }
    if ran == 0 {
        println!("  (no scan cases apply to PostgreSQL {major} — needs PG15+)");
        return;
    }
    assert!(
        failures.is_empty(),
        "scan proofs failed on PostgreSQL {major}:\n{}",
        failures.join("\n")
    );
}

/// The outcome of running a statement while a holder session holds ACCESS SHARE on the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockOutcome {
    /// The op blocked on the holder's ACCESS SHARE and aborted (SQLSTATE 55P03) — it wants a
    /// lock that conflicts with a concurrent reader (ACCESS EXCLUSIVE).
    Blocked,
    /// The op ran to completion despite the held ACCESS SHARE — its lock does not conflict.
    Completes,
}

/// A statement (e.g. VACUUM FULL) that cannot run in a transaction, proven via a blocking-probe:
/// a holder holds ACCESS SHARE and the runner is shown to block (or not) on it. When
/// `expect_rewrite` is set, the op is also run to completion to observe the rewrite.
struct BlockingCase {
    rule: &'static str,
    table: &'static str,
    setup: &'static str,
    op: &'static str,
    expect: BlockOutcome,
    expect_rewrite: Option<RewriteOutcome>,
    pg: RangeInclusive<u32>,
}

fn blocking_cases() -> Vec<BlockingCase> {
    vec![
        BlockingCase {
            rule: "vacuum-full-cluster",
            table: "proof_vacuum_full",
            setup: "CREATE TABLE proof_vacuum_full (c int); \
                    INSERT INTO proof_vacuum_full SELECT g FROM generate_series(1, 100) g;",
            op: "VACUUM FULL proof_vacuum_full",
            expect: BlockOutcome::Blocked,
            expect_rewrite: Some(RewriteOutcome::Changed),
            pg: 14..=18,
        },
        BlockingCase {
            rule: "(control: plain VACUUM does not block a reader)",
            table: "proof_vacuum_full",
            setup: "CREATE TABLE proof_vacuum_full (c int); \
                    INSERT INTO proof_vacuum_full SELECT g FROM generate_series(1, 100) g;",
            op: "VACUUM proof_vacuum_full",
            expect: BlockOutcome::Completes,
            expect_rewrite: None,
            pg: 14..=18,
        },
    ]
}

/// Run one blocking case. A holder takes ACCESS SHARE (open transaction); the runner bounds its
/// wait with `lock_timeout` and runs `op` as its OWN statement, observing block vs completion;
/// then, if `expect_rewrite` is set, runs `op` to completion (no contention) to observe the rewrite.
fn run_blocking_case(
    holder: &mut Client,
    runner: &mut Client,
    case: &BlockingCase,
) -> (BlockOutcome, Option<RewriteOutcome>) {
    let t = case.table;
    runner
        .batch_execute(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
        .expect("drop pre-existing");
    runner.batch_execute(case.setup).expect("setup");

    // Blocking phase: holder holds a reader's lock; runner bounds its wait and runs the op.
    holder.batch_execute("BEGIN").expect("holder begin");
    holder
        .batch_execute(&format!("LOCK TABLE {t} IN ACCESS SHARE MODE"))
        .expect("holder lock");
    runner
        .batch_execute("SET lock_timeout = '1s'")
        .expect("set lock_timeout");
    let outcome = match runner.batch_execute(case.op) {
        Ok(()) => BlockOutcome::Completes,
        Err(e) if e.code() == Some(&postgres::error::SqlState::LOCK_NOT_AVAILABLE) => {
            BlockOutcome::Blocked
        }
        Err(e) => panic!("{}: unexpected error running '{}': {e}", case.rule, case.op),
    };
    holder.batch_execute("ROLLBACK").expect("holder rollback");

    // Rewrite phase: with the lock free, run the op to completion and observe the relfilenode.
    let rewrite = case.expect_rewrite.map(|_| {
        let oid: u32 = runner
            .query_one(&format!("SELECT '{t}'::regclass::oid"), &[])
            .expect("resolve oid")
            .get::<_, u32>(0);
        let before = relfilenode(runner, oid).expect("table exists before the completion run");
        runner
            .batch_execute("SET lock_timeout = '0'")
            .expect("clear lock_timeout");
        runner.batch_execute(case.op).expect("run op to completion");
        classify_rewrite(before, relfilenode(runner, oid))
    });

    runner
        .batch_execute(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
        .expect("drop");
    (outcome, rewrite)
}

#[test]
#[ignore = "requires DATABASE_URL pointing at a throwaway Postgres (run with --ignored)"]
fn statements_block_as_claimed() {
    let mut holder = connect();
    let mut runner = connect();
    let major = server_major(
        runner
            .query_one("SELECT current_setting('server_version_num')::int", &[])
            .expect("read server_version_num")
            .get::<_, i32>(0),
    );

    let mut ran = 0;
    let mut failures = Vec::new();
    println!("\n=== pgsafe blocking proofs (PostgreSQL {major}) ===");
    for case in blocking_cases() {
        if !case.pg.contains(&major) {
            println!("  SKIP {:<42} (out of pg range)", case.rule);
            continue;
        }
        ran += 1;
        let (outcome, rewrite) = run_blocking_case(&mut holder, &mut runner, &case);
        let ok = outcome == case.expect && rewrite == case.expect_rewrite;
        println!(
            "  {} {:<42} {:?} rewrite={:?}",
            if ok { "OK  " } else { "FAIL" },
            case.rule,
            outcome,
            rewrite,
        );
        if outcome != case.expect {
            failures.push(format!(
                "{}: expected {:?}, observed {:?}",
                case.rule, case.expect, outcome
            ));
        }
        if rewrite != case.expect_rewrite {
            failures.push(format!(
                "{}: rewrite expected {:?}, observed {:?}",
                case.rule, case.expect_rewrite, rewrite
            ));
        }
    }
    assert!(ran > 0, "no blocking cases applied to PostgreSQL {major}");
    assert!(
        failures.is_empty(),
        "blocking proofs failed on PostgreSQL {major}:\n{}",
        failures.join("\n")
    );
}

/// A non-rewriting `ALTER COLUMN … TYPE` that nonetheless invalidates a cached plan. Proven by
/// preparing a statement over the column (which fixes its result type), running the change, and
/// showing (a) the table is NOT rewritten and (b) re-executing the prepared statement fails with
/// "cached plan must not change result type". `seed` is a literal that fits `old_type`.
struct CachedPlanCase {
    old_type: &'static str,
    new_type: &'static str,
    seed: &'static str,
    pg: RangeInclusive<u32>,
}

fn cached_plan_cases() -> Vec<CachedPlanCase> {
    vec![
        CachedPlanCase {
            old_type: "varchar(100)",
            new_type: "text",
            seed: "'x'",
            pg: 14..=18,
        },
        CachedPlanCase {
            old_type: "varchar(100)",
            new_type: "varchar(200)",
            seed: "'x'",
            pg: 14..=18,
        },
        CachedPlanCase {
            old_type: "timestamp(0)",
            new_type: "timestamp(6)",
            seed: "now()",
            pg: 14..=18,
        },
        CachedPlanCase {
            old_type: "numeric(10,2)",
            new_type: "numeric(12,2)",
            seed: "1.23",
            pg: 14..=18,
        },
    ]
}

/// Run one cached-plan case on a single session: prepare a statement over the column, run the
/// no-rewrite type change, and return (rewrite outcome, whether the re-execute failed with the
/// cached-plan error).
fn run_cached_plan_case(client: &mut Client, case: &CachedPlanCase) -> (RewriteOutcome, bool) {
    let t = "proof_cached_plan";
    client
        .batch_execute(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
        .expect("drop pre-existing");
    // Clear any prepared statement a prior case left behind, so PREPARE below cannot collide.
    client.batch_execute("DEALLOCATE ALL").ok();
    client
        .batch_execute(&format!(
            "CREATE TABLE {t} (c {}); INSERT INTO {t} VALUES ({});",
            case.old_type, case.seed
        ))
        .expect("setup");

    let oid: u32 = client
        .query_one(&format!("SELECT '{t}'::regclass::oid"), &[])
        .expect("resolve oid")
        .get::<_, u32>(0);
    let before = relfilenode(client, oid).expect("table exists before the change");

    // Prepare + execute once: this fixes the prepared statement's promised result type.
    client
        .batch_execute(&format!("PREPARE pcp AS SELECT c FROM {t}; EXECUTE pcp;"))
        .expect("prepare + first execute");

    // The flagged change must succeed — it is metadata-only.
    client
        .batch_execute(&format!(
            "ALTER TABLE {t} ALTER COLUMN c TYPE {}",
            case.new_type
        ))
        .expect("alter column type");

    let rewrite = classify_rewrite(before, relfilenode(client, oid));

    // Re-execute: a non-rewriting type change still invalidates the cached plan.
    let broke = match client.batch_execute("EXECUTE pcp") {
        Ok(()) => false,
        Err(e) => e
            .as_db_error()
            .map(|d| {
                d.message()
                    .contains("cached plan must not change result type")
            })
            .unwrap_or(false),
    };

    client.batch_execute("DEALLOCATE ALL").ok();
    client
        .batch_execute(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
        .expect("drop");
    (rewrite, broke)
}

#[test]
#[ignore = "requires DATABASE_URL pointing at a throwaway Postgres (run with --ignored)"]
fn no_rewrite_type_changes_break_cached_plans() {
    let mut client = connect();
    let major = server_major(
        client
            .query_one("SELECT current_setting('server_version_num')::int", &[])
            .expect("read server_version_num")
            .get::<_, i32>(0),
    );

    let mut ran = 0;
    let mut failures = Vec::new();
    println!("\n=== pgsafe cached-plan proofs (PostgreSQL {major}) ===");
    for case in cached_plan_cases() {
        if !case.pg.contains(&major) {
            println!(
                "  SKIP {} -> {} (out of pg range)",
                case.old_type, case.new_type
            );
            continue;
        }
        ran += 1;
        let (rewrite, broke) = run_cached_plan_case(&mut client, &case);
        let ok = rewrite == RewriteOutcome::Unchanged && broke;
        println!(
            "  {} {} -> {:<14} rewrite={:?} cached_plan_broke={}",
            if ok { "OK  " } else { "FAIL" },
            case.old_type,
            case.new_type,
            rewrite,
            broke
        );
        if rewrite != RewriteOutcome::Unchanged {
            failures.push(format!(
                "{} -> {}: expected no rewrite, observed {:?}",
                case.old_type, case.new_type, rewrite
            ));
        }
        if !broke {
            failures.push(format!(
                "{} -> {}: expected cached-plan invalidation, but the re-execute succeeded",
                case.old_type, case.new_type
            ));
        }
    }
    assert!(
        ran > 0,
        "no cached-plan cases applied to PostgreSQL {major}"
    );
    assert!(
        failures.is_empty(),
        "cached-plan proofs failed on PostgreSQL {major}:\n{}",
        failures.join("\n")
    );
}
