import { describe, expect, it } from "vitest";
import { actionLabel, organizeDryRunLines } from "./organize-actions";
import type { OrganizeRow } from "~/types";

function row(overrides: Partial<OrganizeRow>): OrganizeRow {
	return {
		input: "/lib/Switch/Game (USA).nsp",
		output: null,
		console: "Switch",
		action: "copy",
		status: "ok",
		planned: true,
		detail: null,
		input_bytes: 1,
		output_bytes: 1,
		elapsed_ms: 1,
		...overrides,
	};
}

describe("actionLabel", () => {
	it("labels plain actions and converter suffixes", () => {
		expect(actionLabel("zip")).toBe("Zip");
		expect(actionLabel("clean")).toBe("Clean");
		expect(actionLabel("dol.compress")).toBe("Compress");
		expect(actionLabel("chd.migrate")).toBe("Migrate");
		expect(actionLabel("ctr.convert")).toBe("Convert");
		expect(actionLabel("nx.decrypt")).toBe("Decrypt");
	});

	it("labels no-op actions with an empty string and passes unknown actions through", () => {
		expect(actionLabel("skip")).toBe("");
		expect(actionLabel("organize")).toBe("");
		expect(actionLabel("something.else")).toBe("something.else");
	});
});

describe("organizeDryRunLines", () => {
	it("shows paths relative to the library and output roots with forward slashes", () => {
		const lines = organizeDryRunLines(
			[
				row({ input: "/lib/Switch\\game.nsp", output: "/out/Switch/game.nsp" }),
			],
			"/lib",
			"/out",
		);
		expect(lines[0]!.source).toBe("Switch/game.nsp");
		expect(lines[0]!.output).toBe("Switch/game.nsp");
	});

	it("keeps the full path for rows outside both roots", () => {
		const lines = organizeDryRunLines(
			[
				row({ input: "/elsewhere/game.nsp" }),
				row({ action: "clean", input: "/lib/stray.nsp", output: "/outside/stale.nsp" }),
			],
			"/lib",
			"/out",
		);
		expect(lines[0]!.source).toBe("/elsewhere/game.nsp");
		expect(lines[1]!.output).toBe("/outside/stale.nsp");
	});

	it("joins the action label and detail into the note", () => {
		const lines = organizeDryRunLines([row({ action: "zip", detail: "new" })], "/lib", "/out");
		expect(lines[0]!.note).toContain("Zip");
		expect(lines[0]!.note).toContain("new");
	});

	it("flags failed, clean and replacement decisions as conflicts", () => {
		const lines = organizeDryRunLines(
			[
				row({ status: "failed", detail: "io error" }),
				row({ action: "clean" }),
				row({ detail: "overwrite" }),
				row({ detail: "rewrite (invalid)" }),
				row({ detail: "rename to /out/Switch/game (1).nsp" }),
				row({ status: "skipped", detail: "output exists" }),
			],
			"/lib",
			"/out",
		);
		expect(lines.map((line) => line.conflict)).toEqual([true, true, true, true, true, true]);
	});

	it("mutes skips that are not conflicts and leaves plain writes alone", () => {
		const lines = organizeDryRunLines(
			[
				row({ status: "skipped", detail: "verified valid" }),
				row({ status: "skipped", detail: "already in place" }),
				row({ detail: "new" }),
			],
			"/lib",
			"/out",
		);
		expect(lines[0]!.muted).toBe(true);
		expect(lines[0]!.conflict).toBe(false);
		expect(lines[1]!.muted).toBe(true);
		expect(lines[2]!.muted).toBe(false);
		expect(lines[2]!.conflict).toBe(false);
	});
});
