//! Guards that the website's committed rule catalog stays in sync with the
//! crate's actual rules. Without this, a new or renamed rule could ship while
//! `site/src/data/rules.catalog.json` is stale — leaving the rule's
//! `/rules/<id>` page missing and the playground's finding link 404-ing, with
//! every other check still green.

use std::collections::BTreeSet;

#[test]
fn site_rule_catalog_matches_crate() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/site/src/data/rules.catalog.json"
    );
    let json = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path}: {e}"));
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    let catalog: BTreeSet<&str> = value["rules"]
        .as_array()
        .expect("rules.catalog.json has a `rules` array")
        .iter()
        .map(|r| r.as_str().expect("each rule id is a string"))
        .collect();
    let crate_ids: BTreeSet<&str> = pgsafe::list_rule_ids().into_iter().collect();

    assert_eq!(
        catalog, crate_ids,
        "site/src/data/rules.catalog.json is out of sync with the crate. Regenerate it:\n  \
         cargo run --release -- --list-rules --format json | python3 -m json.tool > site/src/data/rules.catalog.json"
    );
}
