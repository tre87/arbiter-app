import { defineConfig } from "vite"
import vue from "@vitejs/plugin-vue"
import { fileURLToPath, URL } from "node:url"

export default defineConfig({
  plugins: [vue()],
  clearScreen: false,
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  build: {
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    minify: !process.env.TAURI_ENV_DEBUG,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    // Desktop app: assets are embedded in the binary and loaded from disk, so
    // the default 500 kB chunk warning (a web download-time heuristic) is just
    // noise. Raise it past our main app chunk (~560 kB).
    chunkSizeWarningLimit: 700,
    rollupOptions: {
      output: {
        manualChunks(id) {
          // Do NOT manualChunk `@xterm/*` — Rollup wraps `@xterm/xterm` in an
          // IIFE whose internal `xterm_exports` symbol gets tree-shaken away
          // when the package is split into its own chunk, breaking addon
          // imports at runtime (`SyntaxError: Export 'xterm_exports' is not
          // defined in module`). Default chunking keeps core + addons together.
          if (id.includes("node_modules/@dicebear/")) return "vendor-dicebear"
          if (id.includes("node_modules/vue/") || id.includes("node_modules/@vue/") || id.includes("node_modules/pinia/")) return "vendor-vue"
        },
      },
    },
  },
})
