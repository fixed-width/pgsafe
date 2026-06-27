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

use std::ops::RangeInclusive;

use postgres::{Client, NoTls};

/// One empirical proof: run `ddl` against a freshly-seeded `table` and assert the observed
/// lock mode and rewrite match. `setup` creates and seeds `table` (committed). `pg` is the
/// inclusive major-version range the case applies to.
struct ProofCase {
    rule: &'static str,
    table: &'static str,
    setup: &'static str,
    ddl: &'static str,
    expect_lock: &'static str,
    expect_rewrite: bool,
    pg: RangeInclusive<u32>,
}

/// The v0 proof cases. The final entry is a *control*: a strong-lock statement that does NOT
/// rewrite, proving the rewrite detector discriminates (it must observe `rewrite = false`).
fn cases() -> Vec<ProofCase> {
    vec![
        ProofCase {
            rule: "add-index-non-concurrent",
            table: "proof_add_index",
            setup: "CREATE TABLE proof_add_index (c int); \
                    INSERT INTO proof_add_index SELECT g FROM generate_series(1, 3) g;",
            ddl: "CREATE INDEX proof_add_index_ix ON proof_add_index (c)",
            expect_lock: "ShareLock",
            expect_rewrite: false,
            pg: 14..=18,
        },
        ProofCase {
            rule: "alter-column-type",
            table: "proof_alter_type",
            setup: "CREATE TABLE proof_alter_type (c int); \
                    INSERT INTO proof_alter_type SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_alter_type ALTER COLUMN c TYPE bigint",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: true,
            pg: 14..=18,
        },
        ProofCase {
            rule: "add-column-volatile-default",
            table: "proof_vol_default",
            setup: "CREATE TABLE proof_vol_default (id int); \
                    INSERT INTO proof_vol_default SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_vol_default ADD COLUMN u uuid DEFAULT gen_random_uuid()",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: true,
            pg: 14..=18,
        },
        ProofCase {
            rule: "(control: strong lock, no rewrite)",
            table: "proof_control",
            setup: "CREATE TABLE proof_control (id int); \
                    INSERT INTO proof_control SELECT g FROM generate_series(1, 3) g;",
            ddl: "ALTER TABLE proof_control ADD COLUMN c int",
            expect_lock: "AccessExclusiveLock",
            expect_rewrite: false,
            pg: 14..=18,
        },
    ]
}

/// What the harness observed for one case.
struct Observed {
    lock: String,
    rewrite: bool,
}

/// Connect to `DATABASE_URL` (NoTls — throwaway local/CI Postgres only).
fn connect() -> Client {
    let url = std::env::var("DATABASE_URL")
        .expect("set DATABASE_URL to a throwaway Postgres to run the rule proofs");
    Client::connect(&url, NoTls).expect("connect to DATABASE_URL")
}

/// Read the current `relfilenode` of the relation with the given oid.
fn relfilenode(c: &mut Client, oid: u32) -> u32 {
    c.query_one("SELECT relfilenode FROM pg_class WHERE oid = $1", &[&oid])
        .expect("read relfilenode")
        .get::<_, u32>(0)
}

/// Run one proof case: seed (committed), run the DDL in an open transaction, read the held
/// lock from the observer session and the rewrite from the actor session, then roll back and
/// drop the throwaway table.
fn run_case(actor: &mut Client, observer: &mut Client, case: &ProofCase) -> Observed {
    let t = case.table;
    actor
        .batch_execute(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
        .expect("drop pre-existing");
    actor.batch_execute(case.setup).expect("setup");

    // Stable identifiers for the target table (committed schema).
    let oid: u32 = actor
        .query_one(&format!("SELECT '{t}'::regclass::oid"), &[])
        .expect("resolve table oid")
        .get::<_, u32>(0);
    let pid: i32 = actor
        .query_one("SELECT pg_backend_pid()", &[])
        .expect("backend pid")
        .get::<_, i32>(0);
    let rel_before = relfilenode(actor, oid);

    // Act: run the flagged DDL in an OPEN transaction so the lock stays held.
    actor.batch_execute("BEGIN").expect("begin");
    actor.batch_execute(case.ddl).expect("run flagged ddl");
    let rel_after = relfilenode(actor, oid);

    // Observe the strongest relation lock the actor holds on the table, from a 2nd session.
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
        .unwrap_or_else(|| panic!("no relation lock observed on {t} for backend {pid}"));

    actor.batch_execute("ROLLBACK").expect("rollback");
    actor
        .batch_execute(&format!("DROP TABLE IF EXISTS {t} CASCADE"))
        .expect("drop");

    Observed {
        lock,
        rewrite: rel_after != rel_before,
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
        println!(
            "  {} {:<34} lock={} rewrite={}",
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
                "{}: rewrite expected {}, observed {}",
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
