import { svelte } from "@sveltejs/vite-plugin-svelte";
import { defineConfig } from "vitest/config";

// The build is embedded in the doppel binary and served by its gateway:
// hashed file names under /assets/, nothing inline that the gateway's
// content security policy would block.
export default defineConfig({
  plugins: [svelte()],
  build: {
    assetsInlineLimit: 0,
    modulePreload: { polyfill: false },
    target: "es2022",
  },
  test: {
    include: ["src/**/*.test.ts"],
  },
});
