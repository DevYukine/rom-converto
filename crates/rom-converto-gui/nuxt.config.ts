export default defineNuxtConfig({
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
  },
});
