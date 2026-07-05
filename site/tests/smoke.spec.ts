import { test, expect } from '@playwright/test';

test('landing renders hero, features, and install', async ({ page }) => {
  const errors: string[] = [];
  page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });

  await page.goto('/');
  await expect(page).toHaveTitle(/pgsafe/);
  await expect(page.locator('h1')).toContainText(/without fear/i);
  await expect(page.locator('#features')).toContainText(/No database needed/i);
  await expect(page.locator('#install')).toContainText(/fixed-width\/pgsafe@/);

  // The hero terminal shows a real finding with its severity + rule id.
  await expect(page.locator('.term')).toContainText('add-index-non-concurrent');

  expect(errors, `console errors: ${errors.join('\n')}`).toEqual([]);
});

test('usage doc renders and is active in the sidebar', async ({ page }) => {
  const errors: string[] = [];
  page.on('console', (m) => { if (m.type() === 'error') errors.push(m.text()); });

  await page.goto('/docs/usage/');
  await expect(page.locator('h1')).toContainText(/Usage/i);
  await expect(page.locator('article.prose')).toContainText('add-index-non-concurrent');
  await expect(page.locator('aside nav a.active')).toHaveText(/Usage/i);

  expect(errors, `console errors: ${errors.join('\n')}`).toEqual([]);
});
