import { test, expect } from '@playwright/test';

test('playground lints the seeded migration and links to the rule', async ({ page }) => {
  await page.goto('/playground/');
  const finding = page.locator('.finding', { hasText: 'add-index-non-concurrent' });
  await expect(finding).toBeVisible({ timeout: 20_000 }); // wasm load + lint
  await expect(finding.locator('a')).toHaveAttribute('href', '/rules/add-index-non-concurrent/');
  // Opens in a new tab so the user keeps their migration in the playground.
  await expect(finding.locator('a')).toHaveAttribute('target', '_blank');
});

test('a safe (CONCURRENTLY) example reports no findings', async ({ page }) => {
  await page.goto('/playground/');
  await expect(page.locator('.finding').first()).toBeVisible({ timeout: 20_000 });
  await page.selectOption('#examples', 'concurrent-index');
  await expect(page.locator('#results')).toContainText('No findings');
});

test('the in-transaction example flags concurrently-in-transaction', async ({ page }) => {
  await page.goto('/playground/');
  await expect(page.locator('.finding').first()).toBeVisible({ timeout: 20_000 });
  await page.selectOption('#examples', 'concurrently-in-txn');
  await expect(page.locator('.finding', { hasText: 'concurrently-in-transaction' })).toBeVisible();
  await expect(page.locator('#opt-intx')).toBeChecked();
});

test('a pgsafe:ignore directive marks the finding suppressed', async ({ page }) => {
  await page.goto('/playground/');
  await expect(page.locator('.finding').first()).toBeVisible({ timeout: 20_000 });
  await page.locator('.cm-content').click();
  await page.keyboard.press('ControlOrMeta+A');
  await page.keyboard.type(
    '-- pgsafe:ignore add-index-non-concurrent  maintenance window\nCREATE INDEX idx ON t (col);',
  );
  const suppressed = page.locator('.finding.suppressed', { hasText: 'add-index-non-concurrent' });
  await expect(suppressed).toBeVisible();
  await expect(suppressed.locator('.ignored')).toBeVisible();
});

test('the pgsafe:ignore example loads and shows a suppressed finding', async ({ page }) => {
  await page.goto('/playground/');
  await expect(page.locator('.finding').first()).toBeVisible({ timeout: 20_000 });
  await page.selectOption('#examples', 'ignore-directive');
  await expect(
    page.locator('.finding.suppressed', { hasText: 'add-index-non-concurrent' }),
  ).toBeVisible();
});

test('invalid SQL renders a parse error', async ({ page }) => {
  await page.goto('/playground/');
  await expect(page.locator('.finding').first()).toBeVisible({ timeout: 20_000 });
  await page.locator('.cm-content').click();
  await page.keyboard.press('ControlOrMeta+A');
  await page.keyboard.type('this is not valid sql;');
  await expect(page.locator('#results')).toContainText('Parse error');
  await expect(page.locator('.finding')).toHaveCount(0);
});

test('a permalink hash restores the editor and lints it', async ({ page }) => {
  // Same base64 contract the rule pages use for their "Try it" deep link.
  const hash = Buffer.from(
    JSON.stringify({ sql: 'CREATE INDEX i ON t (c);', inTransaction: false }),
  ).toString('base64');
  await page.goto(`/playground/#${hash}`);
  await expect(page.locator('.cm-content')).toContainText('CREATE INDEX i ON t (c)');
  await expect(
    page.locator('.finding', { hasText: 'add-index-non-concurrent' }),
  ).toBeVisible({ timeout: 20_000 });
});

test('claimed lines are underlined by severity, error winning over warning', async ({ page }) => {
  // Line 1 carries an error (add-index) AND a warning (require-timeout) -> red.
  // Line 2 carries only warnings (drop-column + require-timeout) -> yellow.
  const hash = Buffer.from(
    JSON.stringify({
      sql: 'CREATE INDEX i ON t (a);\nALTER TABLE t DROP COLUMN c;',
      inTransaction: false,
    }),
  ).toString('base64');
  await page.goto(`/playground/#${hash}`);
  await expect(page.locator('.finding').first()).toBeVisible({ timeout: 20_000 });

  const line1 = page.locator('.cm-line').nth(0);
  const line2 = page.locator('.cm-line').nth(1);
  // Mixed-severity line wins as error; warning-only line stays warning.
  await expect(line1).toHaveClass(/cm-claim-error/);
  await expect(line1).not.toHaveClass(/cm-claim-warning/);
  await expect(line2).toHaveClass(/cm-claim-warning/);
  await expect(line2).not.toHaveClass(/cm-claim-error/);

  // The class actually paints a wavy underline in the severity color (--error /
  // --warning), not just a marker class.
  const deco = (el: Element) => {
    const s = getComputedStyle(el);
    return { line: s.textDecorationLine, style: s.textDecorationStyle, color: s.textDecorationColor };
  };
  expect(await line1.evaluate(deco)).toEqual({
    line: 'underline',
    style: 'wavy',
    color: 'rgb(248, 81, 73)', // --error #f85149
  });
  expect(await line2.evaluate(deco)).toEqual({
    line: 'underline',
    style: 'wavy',
    color: 'rgb(227, 179, 65)', // --warning #e3b341
  });
});

test('a clean migration leaves no claim underlines', async ({ page }) => {
  await page.goto('/playground/');
  await expect(page.locator('.finding').first()).toBeVisible({ timeout: 20_000 });
  await expect(page.locator('.cm-claim-error, .cm-claim-warning').first()).toBeVisible();
  await page.selectOption('#examples', 'concurrent-index');
  await expect(page.locator('#results')).toContainText('No findings');
  await expect(page.locator('.cm-claim-error, .cm-claim-warning')).toHaveCount(0);
});

test('fail-on toggles the gate verdict', async ({ page }) => {
  await page.goto('/playground/');
  await expect(page.locator('.finding').first()).toBeVisible({ timeout: 20_000 });
  // Seeded migration trips a warning (require-timeout) and an error.
  await expect(page.locator('.gate')).toContainText('would fail');
  await page.selectOption('#opt-failon', 'never');
  await expect(page.locator('.gate')).toContainText('would pass');
});

test('Fix button applies the fix and clears the finding', async ({ page }) => {
  const hash = Buffer.from(
    JSON.stringify({ sql: 'CREATE INDEX i ON t (a);', inTransaction: false }),
  ).toString('base64');
  await page.goto(`/playground/#${hash}`);
  const finding = page.locator('.finding', { hasText: 'add-index-non-concurrent' });
  await expect(finding).toBeVisible({ timeout: 20_000 });
  await finding.getByRole('button', { name: /CONCURRENTLY|Fix/ }).click();
  // The editor now carries the fix and the finding is gone.
  await expect(page.locator('.cm-content')).toContainText('CREATE INDEX CONCURRENTLY i ON t (a)');
  await expect(page.locator('.finding', { hasText: 'add-index-non-concurrent' })).toHaveCount(0);
});

test('Ignore button inserts a provenance directive and suppresses the finding', async ({ page }) => {
  const hash = Buffer.from(
    JSON.stringify({ sql: 'CREATE INDEX i ON t (a);', inTransaction: false }),
  ).toString('base64');
  await page.goto(`/playground/#${hash}`);
  const finding = page.locator('.finding', { hasText: 'add-index-non-concurrent' });
  await expect(finding).toBeVisible({ timeout: 20_000 });
  await finding.getByRole('button', { name: 'Ignore' }).click();
  await expect(page.locator('.cm-content')).toContainText('pgsafe:ignore add-index-non-concurrent');
  // The finding is now rendered as suppressed (dimmed + "ignored" tag).
  await expect(page.locator('.finding.suppressed', { hasText: 'add-index-non-concurrent' })).toBeVisible();
});

test('multi-byte SQL: Fix applies at the correct offset', async ({ page }) => {
  // A non-ASCII comment before the statement shifts byte offsets vs char indices.
  const hash = Buffer.from(
    JSON.stringify({ sql: '-- café\nCREATE INDEX i ON t (a);', inTransaction: false }),
  ).toString('base64');
  await page.goto(`/playground/#${hash}`);
  await expect(page.locator('.finding', { hasText: 'add-index-non-concurrent' })).toBeVisible({ timeout: 20_000 });
  await page.locator('.finding', { hasText: 'add-index-non-concurrent' }).getByRole('button', { name: /CONCURRENTLY|Fix/ }).click();
  await expect(page.locator('.cm-content')).toContainText('-- café');
  await expect(page.locator('.cm-content')).toContainText('CREATE INDEX CONCURRENTLY i ON t (a)');
});

test('require-timeout shows a single Fix button that clears all its findings', async ({ page }) => {
  const hash = Buffer.from(
    JSON.stringify({ sql: 'CREATE INDEX i ON t (a);\nDROP TABLE other;', inTransaction: false }),
  ).toString('base64');
  await page.goto(`/playground/#${hash}`);
  await expect(page.locator('.finding').first()).toBeVisible({ timeout: 20_000 });
  // Exactly one require-timeout row carries a Fix button (the first flagged finding).
  const timeoutRows = page.locator('.finding', { hasText: 'require-timeout' });
  await expect(timeoutRows.locator('button.fix')).toHaveCount(1);
  await timeoutRows.locator('button.fix').click();
  await expect(page.locator('.cm-content')).toContainText("SET lock_timeout = '5s';");
  await expect(page.locator('.finding', { hasText: 'require-timeout' })).toHaveCount(0);
});

test('suppression-unused finding has no Ignore button and no rule link', async ({ page }) => {
  // A well-formed ignore directive for a real rule, but that rule produces no finding on
  // "SELECT 1;" — pgsafe emits a suppression-unused warning.  Ignoring that meta-diagnostic
  // would itself trip suppression-unknown-rule, so the Ignore button must be absent, and
  // there is no /rules/suppression-unused/ page so the rule id must not be a link.
  const hash = Buffer.from(
    JSON.stringify({
      sql: '-- pgsafe:ignore add-index-non-concurrent  not here\nSELECT 1;',
      inTransaction: false,
    }),
  ).toString('base64');
  await page.goto(`/playground/#${hash}`);
  const finding = page.locator('.finding', { hasText: 'suppression-unused' });
  await expect(finding).toBeVisible({ timeout: 20_000 });
  // No Ignore button on a meta-diagnostic — clicking one would produce suppression-unknown-rule.
  await expect(finding.getByRole('button', { name: 'Ignore' })).toHaveCount(0);
  // Rule id rendered as a plain span, not a link — /rules/suppression-unused/ would 404.
  await expect(finding.locator('a')).toHaveCount(0);
  await expect(finding.locator('span.rule')).toContainText('suppression-unused');
});

test('re-linting clears a stale hover highlight (no phantom highlight on a clean migration)', async ({ page }) => {
  await page.goto('/playground/');
  await expect(page.locator('.finding').first()).toBeVisible({ timeout: 20_000 });
  await page.locator('.finding').first().hover();
  await expect(page.locator('.cm-hl-line')).toHaveCount(1);
  // Loading a safe example removes the hovered row without a mouseleave.
  await page.selectOption('#examples', 'concurrent-index');
  await expect(page.locator('#results')).toContainText('No findings');
  await expect(page.locator('.cm-hl-line')).toHaveCount(0);
});
