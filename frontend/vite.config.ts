import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import tailwindcss from "@tailwindcss/vite";
import { visualizer } from "rollup-plugin-visualizer";

export default defineConfig({
  plugins: [
    // vitest 4 + vite-plugin-solid 2.11.12 兼容修复:
    // solid-refresh babel 插件注入 import "/@solid-refresh" 虚拟模块,
    // vite-plugin-solid 的 resolveId/load 钩子本应处理它,但 vitest 4 的
    // deps optimizer 在插件钩子前用 new URL 把 /@solid-refresh 转成
    // file:///@solid-refresh 触发 TypeError。此插件在 solidPlugin 之前
    // 把虚拟 id 重定向到真实文件,绕过 vitest 的错误转换。
    {
      name: "solid-refresh-virtual-fix",
      enforce: "pre",
      resolveId(id) {
        if (id === "/@solid-refresh") {
          return require.resolve("solid-refresh/dist/solid-refresh.mjs");
        }
        return null;
      },
    },
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
