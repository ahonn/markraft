import { defineConfig } from 'astro/config';

// A local tool: it is never deployed, so it needs no site URL or adapter.
export default defineConfig({
  server: { port: 4331 },
});
