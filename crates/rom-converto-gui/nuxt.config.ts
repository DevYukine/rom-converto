export default defineNuxtConfig({
	compatibilityDate: "2025-01-01",
	ssr: false,
	devtools: {enabled: false},
	telemetry: false,
	modules: ["@pinia/nuxt"],
	components: [{ path: "~/components", pathPrefix: false }],
	hooks: {
		// Nuxt scans types/ for auto-imports, but the generated bindings are imported
		// explicitly and share names with types declared in stores.
		"imports:dirs"(dirs) {
			const types = dirs.findIndex((dir) => dir.endsWith("/types") && !dir.endsWith("/shared/types"));
			if (types !== -1) dirs.splice(types, 1);
		},
	},
	app: {
		head: {
			link: [{ rel: "icon", type: "image/png", href: "/logo.png" }],
		},
	},
	spaLoadingTemplate: "app/spa-loading-template.html",
	experimental: {
		payloadExtraction: false,
	},
	css: ["~/assets/css/tokens.css", "~/assets/css/main.css", "~/assets/css/results.css"],
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
