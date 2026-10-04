import { defineConfig } from "vite";
import { resolve } from "path";

export default defineConfig({
  root: "src",
  build: {
    outDir: "../dist",
    emptyOutDir: true,
    rollupOptions: {
      input: {
        launcher: resolve(__dirname, "src/launcher/index.html"),
        hushlight: resolve(__dirname, "src/hushlight/index.html"),
        screensaver: resolve(__dirname, "src/screensaver/index.html"),
        notification: resolve(__dirname, "src/notification/index.html"),
        tables: resolve(__dirname, "src/tables/index.html"),
        "table-taskbar": resolve(__dirname, "src/table-taskbar/index.html"),
        "table-settings": resolve(__dirname, "src/table-settings/index.html"),
        "table-widgets": resolve(__dirname, "src/table-widgets/index.html"),
        "table-desktop": resolve(__dirname, "src/table-desktop/index.html"),
        note: resolve(__dirname, "src/note/index.html"),
        tutorial: resolve(__dirname, "src/tutorial/index.html"),
      },
    },
  },
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
  },
});
