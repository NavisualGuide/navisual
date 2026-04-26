import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import { resolve } from "path";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [svelte()],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    // Force IPv4 (127.0.0.1) — without this, Vite binds to ::1 (IPv6) only,
    // and on Windows the WebView2 / browser tries 127.0.0.1 first, waits for
    // a 2 s TCP timeout, then falls back to ::1. Multiplied across the dozens
    // of module fetches a SvelteKit dev page makes, this adds 30–60 s to
    // every cold load. Pinning to 127.0.0.1 eliminates the timeout dance.
    host: host || "127.0.0.1",
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
  // Pre-bundle Tauri runtime imports so each page transform doesn't trigger
  // a fresh module-graph walk through node_modules.
  optimizeDeps: {
    include: [
      "@tauri-apps/api/core",
      "@tauri-apps/api/event",
      "@tauri-apps/api/window",
      "@tauri-apps/api/dpi",
      "@tauri-apps/plugin-global-shortcut",
    ],
  },
  build: {
    rollupOptions: {
      input: {
        main: resolve(__dirname, 'index.html'),
        overlay: resolve(__dirname, 'overlay.html')
      }
    }
  }
}));
