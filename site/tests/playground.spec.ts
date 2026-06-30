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
