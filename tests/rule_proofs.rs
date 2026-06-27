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
#[allow(clippy::cast_sign_loss)]
fn server_major(version_num: i32) -> u32 {
    (version_num / 10_000) as u32
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
