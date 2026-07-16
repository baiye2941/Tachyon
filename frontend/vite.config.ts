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
    //
    // 重要:仅限 vitest 环境。dev/build 浏览器会拒绝 file:// 加载导致白屏,
    // 而 vite-plugin-solid 自带 /@solid-refresh 虚拟模块在 dev 下工作正常。
    ...(process.env.VITEST
      ? [
          {
            name: "solid-refresh-virtual-fix",
            enforce: "pre" as const,
            resolveId(id: string) {
              if (id === "/@solid-refresh") {
                return pathToFileURL(
                  require.resolve("solid-refresh/dist/solid-refresh.mjs"),
                ).href;
              }
              return null;
            },
          },
        ]
      : []),
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
          // 审计 FT-16 修正:仅拆 node_modules vendor。
          // 禁止用 path includes 把懒加载面板打成独立 chunk——rolldown/vite
          // 会把共享模块(含 solid-js/stores)吸入 panel-command,导致 index
          // 静态 import 面板 chunk,白屏或破坏真正 lazy。
          if (!id.includes("node_modules")) return undefined;
          const vendorLibs = [
            "solid-js",
            "@tauri-apps/api",
            "@tauri-apps/plugin-dialog",
            "@tauri-apps/plugin-notification",
            "@motionone/solid",
            "@tanstack/solid-virtual",
          ];
          if (vendorLibs.some((lib) => id.includes(lib))) {
            return "vendor";
          }
          return undefined;
        },
      },
    },
  },
  test: {
    environment: "jsdom",
    exclude: ["e2e/**", "node_modules/**"],
    // 部分组件测试（如 DetailPanel）在并行满载运行时容易超时，
    // 提升到 10s 保证在普通开发机上稳定通过。
    testTimeout: 10000,
  },
});
