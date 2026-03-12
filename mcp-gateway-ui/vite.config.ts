import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "fs";
import { resolve } from "path";

// 优先使用 CI 注入的 tag（如 v0.2.0），否则回退到 package.json 的 version
const pkgVersion = (JSON.parse(
  readFileSync(resolve(__dirname, "package.json"), "utf-8")
) as { version: string }).version;
const appVersion = process.env.VITE_APP_VERSION || pkgVersion;

export default defineConfig(async () => ({
  plugins: [react()],
  define: {
    // 编译时注入版本号，运行时通过 import.meta.env.VITE_APP_VERSION 读取
    "import.meta.env.VITE_APP_VERSION": JSON.stringify(appVersion),
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    target:
      process.env.TAURI_ENV_PLATFORM == "windows"
        ? "chrome105"
        : "safari13",
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
}));
