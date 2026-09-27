import { defineConfig, envField } from 'astro/config';
import sitemap from '@astrojs/sitemap';
import tailwindcss from '@tailwindcss/vite';

export default defineConfig({
  site: 'https://markraft.app',
  output: 'static',
  integrations: [sitemap()],
  env: {
    schema: {
      // Analytics is off unless both are set at build time.
      PLAUSIBLE_DOMAIN: envField.string({ context: 'server', access: 'public', optional: true }),
      PLAUSIBLE_SCRIPT_URL: envField.string({ context: 'server', access: 'public', optional: true, url: true }),
    },
  },
  vite: {
    plugins: [tailwindcss()],
  },
});
