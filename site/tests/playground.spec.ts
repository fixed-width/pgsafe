import { test, expect } from '@playwright/test';

test('playground lints the seeded migration and links to the rule', async ({ page }) => {
  await page.goto('/playground/');
  const finding = page.locator('.finding', { hasText: 'add-index-non-concurrent' });
  await expect(finding).toBeVisible({ timeout: 20_000 }); // wasm load + lint
  await expect(finding.locator('a')).toHaveAttribute('href', '/rules/add-index-non-concurrent/');
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
