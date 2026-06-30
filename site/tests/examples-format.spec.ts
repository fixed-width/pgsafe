import { test, expect } from '@playwright/test';
import { RULE_LIST } from '../src/data/rules';

// The rule examples are hand-authored SQL strings — no formatter is run over
// them — so a statement can drift into being written two different ways (e.g.
// the ATTACH that was a one-liner under "Unsafe" but wrapped under "Safe").
// This guard catches exactly that: any statement that appears in BOTH the
// unsafe and safe block of a rule must be rendered identically in both. It does
// NOT impose a house style; it only flags the same statement formatted two ways.

/** Drop `-- ...` line comments (keeping the newlines) so statement boundaries
 *  and identity ignore prose. No example puts `--` inside a string literal. */
function stripComments(sql: string): string {
  return sql
    .split('\n')
    .map((line) => line.replace(/--.*$/, ''))
    .join('\n');
}

/** Split into statements on `;`, each trimmed but with internal line breaks
 *  (the actual wrapping we want to compare) preserved. */
function statements(sql: string): string[] {
  return stripComments(sql)
    .split(';')
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

/** Whitespace-insensitive identity: the same statement regardless of wrapping. */
const identity = (stmt: string): string => stmt.replace(/\s+/g, ' ').trim();

/** Returns the statements that appear in both blocks but are formatted
 *  differently — empty when the two blocks agree. */
export function inconsistentStatements(
  unsafe: string,
  safe: string,
): Array<{ unsafe: string; safe: string }> {
  const byIdentity = new Map<string, string>();
  for (const s of statements(unsafe)) byIdentity.set(identity(s), s);
  const out: Array<{ unsafe: string; safe: string }> = [];
  for (const s of statements(safe)) {
    const twin = byIdentity.get(identity(s));
    if (twin !== undefined && twin !== s) out.push({ unsafe: twin, safe: s });
  }
  return out;
}

for (const rule of RULE_LIST) {
  const ex = rule.example;
  if (!ex?.safe) continue;
  test(`${rule.id}: a statement shared by both blocks is formatted the same way`, () => {
    expect(inconsistentStatements(ex.unsafe, ex.safe!)).toEqual([]);
  });
}

// Prove the guard actually fires on the bug it exists to prevent: the original
// attach-partition shape, where the same ATTACH was a one-liner in one block and
// wrapped in the other.
test('guard flags the same statement formatted two ways (regression shape)', () => {
  const unsafe =
    "ALTER TABLE measurement ATTACH PARTITION m_y2021 FOR VALUES FROM ('a') TO ('b');";
  const safe = [
    'ALTER TABLE measurement ATTACH PARTITION m_y2021',
    "  FOR VALUES FROM ('a') TO ('b');",
  ].join('\n');
  expect(inconsistentStatements(unsafe, safe)).toHaveLength(1);
});
