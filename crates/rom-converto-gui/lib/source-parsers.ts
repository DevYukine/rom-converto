/// <reference types="node" />
import { readFileSync } from "node:fs";

// Read by the drift tests only: they compare what the frontend sends against
// the Rust sources that define what the backend accepts, so a rename on either
// side fails a test instead of failing at runtime.

/** Command names `main.rs` passes to `tauri::generate_handler!`. */
export function registeredCommands(): string[] {
	const src = readFileSync(new URL("../src-tauri/src/main.rs", import.meta.url), "utf8");
	const block = src.match(/generate_handler!\[([\s\S]*?)\]/)?.[1];
	if (!block) throw new Error("no generate_handler! block in src-tauri/src/main.rs");
	return block
		.split(",")
		.map((c) => c.trim())
		.filter(Boolean);
}

/** Field names of the generated `RunOptions` type, the only option keys the
 *  runner accepts (it rejects unknown ones). */
export function runOptionKeys(): string[] {
	const src = readFileSync(new URL("../types/generated/runner.ts", import.meta.url), "utf8");
	const body = src.match(/export type RunOptions = \{([\s\S]*?)\};/)?.[1];
	if (!body) throw new Error("no RunOptions type in types/generated/runner.ts");
	// ts-rs carries the Rust doc comments through as block comments.
	const fields = body.replace(/\/\*[\s\S]*?\*\//g, "");
	return [...fields.matchAll(/(?:^|,)\s*([A-Za-z_][A-Za-z0-9_]*)\s*:/g)].map((m) => m[1]!);
}
