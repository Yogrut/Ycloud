import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vitest/config'

const backend = 'http://127.0.0.1:18473'

export default defineConfig({
  plugins: [vue()],
  base: '/v2/',
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      '/api': { target: backend },
      '/browse': { target: backend },
      '/admin': { target: backend },
      '/preview.html': { target: backend },
      '/favicon.svg': { target: backend },
    },
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    manifest: true,
    sourcemap: false,
  },
  test: {
    environment: 'happy-dom',
    restoreMocks: true,
  },
})
