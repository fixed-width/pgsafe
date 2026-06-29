import { test, expect } from '@playwright/test';
import { readFileSync } from 'node:fs';

const catalog = JSON.parse(
  readFileSync(new URL('../src/data/rules.catalog.json', import.meta.url), 'utf8'),
) as { rules: string[] };

test('rules index lists every rule and links to its page', async ({ page }) => {
  await page.goto('/rules/');
  await expect(page.locator('.rule')).toHaveCount(catalog.rules.length);
});

test('a rule page renders why-unsafe and safe-rewrite', async ({ page }) => {
  await page.goto('/rules/add-index-non-concurrent/');
  await expect(page.locator('h1')).toContainText('add-index-non-concurrent');
  await expect(page.getByText("Why it's unsafe")).toBeVisible();
  await expect(page.getByText('Safe rewrite')).toBeVisible();
});

test('nav exposes Rules and Docs', async ({ page }) => {
  await page.goto('/');
  await expect(page.locator('header.nav')).toContainText('Rules');
  await expect(page.locator('header.nav')).toContainText('Docs');
});
