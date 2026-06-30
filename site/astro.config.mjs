import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';

// Static site served from a custom subdomain (root base).
export default defineConfig({
  site: 'https://pgsafe.fixedwidth.tech',
  output: 'static',
  integrations: [sitemap()],
  // Dark-only site: pin a near-black Shiki theme (default github-dark uses a
  // lighter #24292e panel that reads like a light-mode box on our surface).
  markdown: {
    shikiConfig: { theme: 'github-dark-default' },
  },
});
