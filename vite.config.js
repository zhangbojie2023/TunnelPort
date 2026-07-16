import { defineConfig } from "vite";

// https://vitejs.dev/config/
export default defineConfig({
  // Prevent Vite from clearing the terminal so Tauri logs remain visible
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
  // Expose TAURI_ and VITE_ env vars to the frontend
  envPrefix: ["VITE_", "TAURI_"],
  build: {
    // Tauri v2 requires macOS 10.15+ (WebKit supports ES2020+)
    target: ["es2020"],
    minify: !process.env.TAURI_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_DEBUG,
  },
});
