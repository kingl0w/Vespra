import { defineConfig } from "vite";
import preact from "@preact/preset-vite";

import { cloudflare } from "@cloudflare/vite-plugin";

export default defineConfig({
  plugins: [preact(), cloudflare()],
  server: {
    proxy: {
      "/api": "http://127.0.0.1:9200",
    },
  },
  build: {
    outDir: "dist",
  },
});