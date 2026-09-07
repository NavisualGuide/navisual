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
    // 4. Transform the app's own modules at server start instead of on the
    // webview's first request. `tauri dev` boots a FRESH Vite every run, so the
    // first page load has always paid the whole cold transform, and it is the
    // dominant cost of starting dev: measured on a real launch (2026-09-07),
    // navigationStart at process+1181ms, then ttfb 4067ms, domInteractive
    // 4385ms and DOMContentLoaded 20242ms -- i.e. ~16s spent fetching and
    // transforming the module graph after the HTML was parsed. Release never
    // pays it (bundled assets, custom protocol), which is the whole gap between
    // an instant .exe and a 10-30s dev start.
    //
    // Measured A/B, two cold `tauri dev` launches each, process start to the
    // frontend's first invoke: OFF 21.5s / 25.9s, ON 16.8s / 16.0s. Warmup also
    // removes the variance, which is the tell that the cold transform had been
    // racing the page load. It costs ~0.6-1.1s of extra TTFB on index.html
    // (warmup competes with the first request) and pays back 4-10s at DCL.
    // It does NOT fix the remaining ~10s between domInteractive and DCL, which
    // is the webview fetching and executing 43 dev modules -- Chrome does the
    // same graph warm in ~0.9s, so that residual is still unexplained.
    warmup: {
      clientFiles: ["./src/main.ts", "./src/overlay.ts", "./src/*.svelte", "./src/lib/*.ts"],
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
