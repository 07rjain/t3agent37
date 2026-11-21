import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Two pages: the site, and demo.html, the desktop app's UI on its mock backend (iframed by the site).
// Relative base (`./`) ensures everything works on GitHub Pages (`/t3agent37/`) and root domains alike.
export default defineConfig({
  base: "./",
  plugins: [react()],
  build: {
    rollupOptions: {
      input: { index: "index.html", demo: "demo.html" },
    },
  },
});
