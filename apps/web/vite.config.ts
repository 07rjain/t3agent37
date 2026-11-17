import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Two pages: the site, and demo.html, the desktop app's UI on its mock backend (iframed by the site).
export default defineConfig({
  plugins: [react()],
  build: {
    rollupOptions: {
      input: { index: "index.html", demo: "demo.html" },
    },
  },
});
