import { beforeEach, describe, expect, it } from "vitest";
import { registeredCommands, runOptionKeys } from "../source-parsers";
import { buildCliCommand, setWindowsQuoting } from "~/composables/useCliEcho";
import { allOpDefs, opCommand } from "./index";
import { pageEchoArgs, type RunPayload, type StagedItem } from "./types";

const ITEM: StagedItem = {
	id: "item-1",
	path: "/roms/sample.iso",
	name: "sample.iso",
	size: 1024,
	outExt: "chd",
};

function payloadsOf(def: ReturnType<typeof allOpDefs>[number]): RunPayload[] {
	const store = def.useStore();
	const payloads = [def.buildArgs(store, ITEM, "task-1")];
	if (def.buildArgsAll) payloads.push(def.buildArgsAll(store, [ITEM], "task-1"));
	return payloads;
}

const defs = allOpDefs();

describe("op registry", () => {
	it("registers every op module", () => {
		expect(defs.length).toBeGreaterThan(0);
	});

	it("lets CHD extraction choose its output format beside the input", () => {
		const def = defs.find((def) => def.storeId === "chd-extract")!;
		const item = { ...ITEM, path: "/roms/Game (USA).chd" };
		const { request } = def.buildArgs(def.useStore(), item, "task-1");
		expect(request.output).toBeNull();
		expect(request.options.output_dir).toBe("/roms");
	});

	it("never sends rename for pages whose output is a directory", () => {
		for (const id of [
			"ctr-unbundle",
			"nx-split",
			"wup-decrypt",
			"xenon-convert",
			"xbox-extract",
			"xenon-extract",
			"psp-extract",
			"vita-extract",
		]) {
			const def = defs.find((def) => def.storeId === id);
			expect(def, id).toBeDefined();
			if (!def) continue;
			const store = def.useStore();
			store.onConflict = "rename";
			const { request } = def.buildArgs(store, ITEM, "task-1");
			expect(request.options.on_conflict).not.toBe("rename");
		}
	});

	// The runner rejects unknown option keys outright, so a typo here would only
	// surface as a failed run.
	it("sends only keys the runner's RunOptions defines", () => {
		const known = new Set(runOptionKeys());
		expect(known.size).toBeGreaterThan(0);
		const unknown: string[] = [];
		for (const def of defs) {
			for (const payload of payloadsOf(def)) {
				for (const key of Object.keys(payload.request.options)) {
					if (!known.has(key)) unknown.push(`${def.op}/${def.console}: ${key}`);
				}
			}
		}
		expect(unknown).toEqual([]);
	});

	it("invokes only commands the Tauri backend registers", () => {
		const registered = new Set(registeredCommands());
		const missing = defs
			.filter((def) => !registered.has(opCommand(def, def.useStore())))
			.map((def) => `${def.op}/${def.console}`);
		expect(missing).toEqual([]);
	});
});

describe("page echo parity", () => {
	// The POSIX-quote expectations below must not depend on the host OS.
	beforeEach(() => setWindowsQuoting(false));

	const items: StagedItem[] = [
		{ ...ITEM, id: "a", path: "/roms/Base.nsp", name: "Base.nsp" },
		{ ...ITEM, id: "b", path: "/roms/Update.nsp", name: "Update.nsp" },
	];

	it("echoes every staged input for defs that enqueue one combined job", () => {
		const combined = defs.filter((def) => def.buildArgsAll);
		expect(combined.length).toBeGreaterThan(0);
		for (const def of combined) {
			const echo = buildCliCommand(pageEchoArgs(def, def.useStore(), items, items[0]!, "task-1"));
			for (const item of items) expect(echo, def.storeId).toContain(item.path);
		}
	});
});
