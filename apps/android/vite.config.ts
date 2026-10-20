import path from 'node:path'
import { fileURLToPath } from 'node:url'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import { defineConfig } from 'vite'

const here = path.dirname(fileURLToPath(import.meta.url))

// On a device the dev server must be reachable from the phone; TAURI_DEV_HOST is set by `tauri android dev`.
const host = process.env.TAURI_DEV_HOST

export default defineConfig({
  plugins: [react(), tailwindcss()],
  // @cooldown/ui is consumed as source; its files import each other through '@/'.
  resolve: { alias: { '@': path.resolve(here, '../../packages/ui/src') } },
  clearScreen: false,
  server: { host: host || '127.0.0.1', port: 1421, strictPort: true },
  build: { target: 'es2022', sourcemap: false, outDir: 'dist' },
})
