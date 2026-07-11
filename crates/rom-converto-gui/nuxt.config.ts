export default defineNuxtConfig({
	compatibilityDate: "2025-01-01",
	ssr: false,
	devtools: {enabled: false},
	telemetry: false,
	modules: ["@pinia/nuxt"],
	components: [{ path: "~/components", pathPrefix: false }],
	spaLoadingTemplate: "app/spa-loading-template.html",
	experimental: {
		payloadExtraction: false,
	},
	css: ["~/assets/css/tokens.css", "~/assets/css/main.css"],
	devServer: {
		host: "localhost",
		port: 3001,
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
