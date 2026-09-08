import { describe, expect, it } from "vitest";
import { registeredCommands, runOptionKeys } from "../source-parsers";
import { allOpDefs, opCommand } from "./index";
import type { RunPayload, StagedItem } from "./types";

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

describe("op registry", () => {
	const defs = allOpDefs();

	it("registers every op module", () => {
		expect(defs.length).toBeGreaterThan(0);
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
