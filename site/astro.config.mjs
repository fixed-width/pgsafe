import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';

// Static site served from a custom subdomain (root base).
export default defineConfig({
  site: 'https://pgsafe.fixedwidth.tech',
  output: 'static',
  integrations: [sitemap()],
});
