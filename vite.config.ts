import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [react()],

  // The pinned WebRTC checkout under `.webrtc/` carries thousands of Chromium test fixtures, so
  // test discovery is a whitelist rather than a set of exclusions.
  test: {
    include: ["src/**/*.test.{ts,tsx}"],
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // Cargo and native build trees live at the workspace root, outside
      // `src-tauri`. Watching them can exhaust WebView startup with tens of
      // thousands of file handles while `cargo` is building.
      ignored: [
        "**/src-tauri/**",
        "**/target/**",
        "**/.webrtc/**",
        "**/artifacts/**",
        "**/crates/**",
        "**/native/**",
        "**/apps/**",
        "**/docs/**",
        "**/scripts/**",
        "**/dist/**",
        "**/.tmp/**",
      ],
    },
  },
}));
