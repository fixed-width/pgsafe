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

test('attach-partition shows a safe-rewrite example', async ({ page }) => {
  await page.goto('/rules/attach-partition/');
  await expect(page.locator('.safe-lbl')).toBeVisible();
  await expect(page.locator('pre').last()).toContainText('VALIDATE CONSTRAINT');
});

test('add-column-serial shows a safe-rewrite example', async ({ page }) => {
  await page.goto('/rules/add-column-serial/');
  await expect(page.locator('.safe-lbl')).toBeVisible();
  await expect(page.locator('pre').last()).toContainText('SET DEFAULT nextval');
});

test('rule prose renders inline code, not literal backticks', async ({ page }) => {
  await page.goto('/rules/require-timeout/');
  await expect(page.locator('.md code').filter({ hasText: "SET lock_timeout = '5s';" })).toBeVisible();
  // no raw markdown backticks left anywhere in the page body
  await expect(page.locator('main')).not.toContainText('`');
});
