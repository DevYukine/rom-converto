export default defineNuxtConfig({
  compatibilityDate: "2025-01-01",
  future: { compatibilityVersion: 3 },
  ssr: false,
  devtools: { enabled: false },
  modules: ["@pinia/nuxt"],
  spaLoadingTemplate: "app/spa-loading-template.html",
  experimental: {
    payloadExtraction: false,
  },
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
      force: true,
    },
  },
});
