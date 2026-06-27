use std::hint::black_box;

use criterion::{criterion_group, criterion_main, Criterion};
use pgsafe::{lint_sql, LintOptions};

fn bench_lint(c: &mut Criterion) {
    let small = "CREATE INDEX i ON t (x);";
    let medium = "ALTER TABLE t ADD CONSTRAINT fk FOREIGN KEY (a) REFERENCES u (id);\n".repeat(50);
    let large = "ALTER TABLE t ALTER COLUMN a SET NOT NULL;\n".repeat(1000);
    let opts = LintOptions::default();

    c.bench_function("lint_small", |b| {
        b.iter(|| lint_sql(black_box(small), &opts).unwrap())
    });
    c.bench_function("lint_medium_50", |b| {
        b.iter(|| lint_sql(black_box(medium.as_str()), &opts).unwrap())
    });
    c.bench_function("lint_large_1000", |b| {
        b.iter(|| lint_sql(black_box(large.as_str()), &opts).unwrap())
    });
}

criterion_group!(benches, bench_lint);
criterion_main!(benches);
