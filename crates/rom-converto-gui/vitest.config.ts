import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

// The op registry pulls in the Pinia stores, which rely on Nuxt's `~` alias and
// its auto-imported Vue helpers. Tests get both so they can drive the real defs
// instead of stand-ins.
export default defineConfig({
	resolve: {
		alias: { "~": fileURLToPath(new URL(".", import.meta.url)) },
	},
	test: {
		setupFiles: ["./vitest.setup.ts"],
	},
});
