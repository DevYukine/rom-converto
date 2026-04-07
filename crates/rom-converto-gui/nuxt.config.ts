export default defineNuxtConfig({
  compatibilityDate: "2025-01-01",
  ssr: false,
  devtools: { enabled: false },
  modules: ["@pinia/nuxt"],
  css: ["~/assets/css/main.css"],
  devServer: {
    host: "localhost",
    port: 3000,
  },
  postcss: {
    plugins: {
      "@tailwindcss/postcss": {},
    },
  },
  vite: {
    clearScreen: false,
    server: {
      strictPort: true,
    },
    optimizeDeps: {
      include: [
        "@tauri-apps/api/webview",
        "@tauri-apps/api/core",
        "@tauri-apps/api/event",
        "@tauri-apps/plugin-dialog",
      ],
    },
  },
});
