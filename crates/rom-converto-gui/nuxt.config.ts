export default defineNuxtConfig({
  ssr: false,
  devtools: { enabled: false },
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
