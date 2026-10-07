import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'
import { fileURLToPath, URL } from 'node:url'
import { defineConfig } from 'vitest/config'
import { renderEmbeddedAssets } from './build/embeddedAssets.ts'

const backend = 'http://127.0.0.1:18473'

export default defineConfig({
  plugins: [vue(), tailwindcss(), {
    name: 'ycloud-embedded-assets',
    enforce: 'post',
    generateBundle: {
      order: 'post',
      handler(_options, bundle) {
        this.emitFile({
          type: 'asset',
          fileName: 'embedded-assets.rs',
          source: renderEmbeddedAssets(Object.keys(bundle).filter(path => path.startsWith('assets/'))),
        })
      },
    },
  }],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
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
    // Keep one eagerly loaded stylesheet; page code is still loaded on demand.
    cssCodeSplit: false,
    rollupOptions: {
      output: {
        entryFileNames: 'assets/app.js',
        chunkFileNames: 'assets/[name]-[hash].js',
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
