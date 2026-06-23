import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import tailwindcss from "@tailwindcss/vite";
import { visualizer } from "rollup-plugin-visualizer";

export default defineConfig({
  plugins: [
    solidPlugin(),
    tailwindcss(),
    process.env.ANALYZE === "true"
      ? visualizer({
          open: false,
          gzipSize: true,
          brotliSize: false,
          filename: "dist/stats.html",
        })
      : undefined,
  ].filter(Boolean),
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
    host: "127.0.0.1",
    watch: {
      usePolling: true,
      interval: 300,
    },
  },
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    target: "esnext",
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
    rollupOptions: {
      output: {
        manualChunks(id) {
          if (id.includes("node_modules")) {
            const vendorLibs = [
              "solid-js",
              "@tauri-apps/api",
              "@tauri-apps/plugin-dialog",
              "@tauri-apps/plugin-notification",
              "@motionone/solid",
            ];
            if (vendorLibs.some((lib) => id.includes(lib))) {
              return "vendor";
            }
          }
          const panelModules = [
            "/components/SnifferPanel",
            "/components/HistoryPanel",
            "/components/SettingsPanel",
            "/components/NewTaskModal",
            "/components/CommandPalette",
          ];
          if (panelModules.some((module) => id.includes(module))) {
            return "panels";
          }
          return undefined;
        },
      },
    },
  },
  test: {
    exclude: ["e2e/**", "node_modules/**"],
  },
});
