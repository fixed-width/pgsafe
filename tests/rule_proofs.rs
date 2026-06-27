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

/// One empirical proof. `setup` creates and seeds the objects (committed); `table` is the root
/// object dropped for cleanup (CASCADE removes its dependents); `watch` is the relation whose lock
/// + relfilenode are observed (often `table`, but e.g. the index for REINDEX or the matview for
///   REFRESH). `pg` is the inclusive major-version range the case applies to.
struct ProofCase {
    rule: &'static str,
    table: &'static str,
    watch: &'static str,
    setup: &'static str,
    ddl: &'static str,
    expect_lock: &'static str,
    expect_rewrite: RewriteOutcome,
    pg: RangeInclusive<u32>,
}

/// The v0 proof cases. The final entry is a *control*: a strong-lock statement that does NOT
/// rewrite, proving the rewrite detector discriminates (it must observe `rewrite = Unchanged`).
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
            pg: 14..=18,
        },
    ]
}

/// What the harness observed for one case.
struct Observed {
    lock: String,
    rewrite: RewriteOutcome,
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
    let pid: i32 = actor
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("backend pid")
        .get::<_, i32>(0);
    let rel_before = relfilenode(actor, oid).expect("watched relation exists before the ddl");

    // Act: run the flagged DDL in an OPEN transaction so the lock stays held.
    actor.batch_execute("BEGIN").expect("begin");
    actor.batch_execute(case.ddl).expect("run flagged ddl");
    let rewrite = classify_rewrite(rel_before, relfilenode(actor, oid));

    // Observe the strongest relation lock the actor holds on the watched relation.
    let lock = observer
        .query(
            "SELECT mode FROM pg_locks \
             WHERE pid = $1 AND locktype = 'relation' AND relation = $2 AND granted",
            &[&pid, &oid],
        )
        .expect("read pg_locks")
        .iter()
        .map(|r| r.get::<_, String>(0))
        .max_by_key(|m| lock_strength(m))
        .unwrap_or_else(|| {
            panic!(
                "no relation lock observed on {} for backend {pid}",
                case.watch
            )
        });

    actor.batch_execute("ROLLBACK").expect("rollback");
    actor
        .batch_execute(&format!("DROP TABLE IF EXISTS {root} CASCADE"))
        .expect("drop");

    Observed { lock, rewrite }
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
        println!(
            "  {} {:<34} lock={} rewrite={:?}",
            if lock_ok && rewrite_ok {
                "OK  "
            } else {
                "FAIL"
            },
            case.rule,
            obs.lock,
            obs.rewrite,
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
    }
    assert!(ran > 0, "no proof cases applied to PostgreSQL {major}");
    assert!(
        failures.is_empty(),
        "rule proofs failed on PostgreSQL {major}:\n{}",
        failures.join("\n")
    );
}
