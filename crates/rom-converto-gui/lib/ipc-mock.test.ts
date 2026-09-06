/// <reference types="node" />
import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { handlers } from "./ipc-mock";

// Commands the mock deliberately serves with the convert-family fallback:
// fake progress on the job's key, then a plausible RunOutcome.
const GENERIC_FALLBACK = new Set([
	"cmd_cdn_to_cia",
	"cmd_decrypt_rom",
	"cmd_encrypt_rom",
	"cmd_compress_rom",
	"cmd_decompress_rom",
	"cmd_chd_compress",
	"cmd_chd_migrate",
	"cmd_cso_compress",
	"cmd_cso_to_chd",
	"cmd_cso_decompress",
	"cmd_chd_extract",
	"cmd_chd_to_cso",
	"cmd_cue_merge",
	"cmd_cue_to_iso",
	"cmd_cue_to_cso",
	"cmd_convert_ctr",
	"cmd_compress_disc",
	"cmd_decompress_disc",
	"cmd_wup_compress",
	"cmd_wup_decrypt",
	"cmd_ps3_decrypt",
	"cmd_nds_encrypt",
	"cmd_nds_decrypt",
	"cmd_nx_compress",
	"cmd_nx_decompress",
	"cmd_nx_merge",
	"cmd_nx_split",
	"cmd_xbox_convert",
	"cmd_xbox_extract",
	"cmd_xenon_compress",
	"cmd_xenon_convert",
	"cmd_xenon_extract",
	"cmd_psp_to_iso",
	"cmd_psp_extract",
	"cmd_vita_extract",
]);

function registeredCommands(): string[] {
	const src = readFileSync(new URL("../src-tauri/src/main.rs", import.meta.url), "utf8");
	const block = src.match(/generate_handler!\[([\s\S]*?)\]/)?.[1];
	if (!block) throw new Error("no generate_handler! block in src-tauri/src/main.rs");
	return block
		.split(",")
		.map((c) => c.trim())
		.filter(Boolean);
}

describe("ipc-mock", () => {
	it("covers every command the Tauri backend registers", () => {
		const registered = registeredCommands();
		expect(registered.length).toBeGreaterThan(0);
		const uncovered = registered.filter((cmd) => !(cmd in handlers) && !GENERIC_FALLBACK.has(cmd));
		expect(uncovered).toEqual([]);
	});
});
