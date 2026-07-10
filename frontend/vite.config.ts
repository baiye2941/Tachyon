import { defineConfig } from "vite";
import solidPlugin from "vite-plugin-solid";
import tailwindcss from "@tailwindcss/vite";
import { visualizer } from "rollup-plugin-visualizer";
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

// ESM 下无 require,用 createRequire 桥接(同步语义,与 resolveId 契约一致)。
// import.meta.resolve 返回 Promise,无法在同步 resolveId 中使用。
const require = createRequire(import.meta.url);

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
          // pathToFileURL:Windows 下 require.resolve 返回带反斜杠的绝对路径,
          // rollup 的 resolveId 钩子约定返回 file:// URL 或绝对路径均可,
          // 统一转 file:// URL 避免跨平台路径分隔符差异。
          return pathToFileURL(
            require.resolve("solid-refresh/dist/solid-refresh.mjs")
          ).href;
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
    environment: "jsdom",
    exclude: ["e2e/**", "node_modules/**"],
  },
});
