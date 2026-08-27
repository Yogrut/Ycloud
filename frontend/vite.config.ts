import vue from '@vitejs/plugin-vue'
import { defineConfig } from 'vitest/config'

const backend = 'http://127.0.0.1:18473'

export default defineConfig({
  plugins: [vue()],
  base: '/',
  server: {
    port: 5173,
    strictPort: true,
    proxy: {
      '/api': { target: backend },
      '/dav': { target: backend },
      '/favicon.svg': { target: backend },
    },
  },
  build: {
    outDir: '../static/app',
    emptyOutDir: true,
    rollupOptions: {
      output: {
        entryFileNames: 'assets/app.js',
        chunkFileNames: 'assets/[name].js',
        assetFileNames: asset => asset.name?.endsWith('.css') ? 'assets/app.css' : 'assets/[name][extname]',
      },
    },
    sourcemap: false,
  },
  test: {
    environment: 'happy-dom',
    restoreMocks: true,
  },
})
