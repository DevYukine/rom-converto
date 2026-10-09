import { beforeEach, describe, expect, it } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { buildCliCommand, setWindowsQuoting } from "~/composables/useCliEcho";
import { toolOps } from "./tools";
import type { OpDef, OpStore, StagedItem } from "./types";

// The POSIX-quote expectations below must not depend on the host OS.
beforeEach(() => {
	setWindowsQuoting(false);
	setActivePinia(createPinia());
});

const defByStoreId = Object.fromEntries(toolOps.map((def) => [def.storeId, def])) as Record<string, OpDef>;

function item(path: string): StagedItem {
	return { id: "t1", path, name: path.slice(path.lastIndexOf("/") + 1), size: 0, outExt: "", dir: false };
}

function argsFor(storeId: string, input: string, mutate: (store: OpStore) => void) {
	const def = defByStoreId[storeId]!;
	const store = def.useStore();
	mutate(store);
	return def.buildArgs(store, item(input), "t1");
}

describe("tool output rows", () => {
	it("falls back to the runner defaults when neither file nor directory is set", () => {
		const merge = argsFor("cue-merge", "/lib/game.cue", () => {});
		expect(merge.request.output).toBe("/lib/game (merged).cue");

		const cdn = argsFor("ctr-cdn-to-cia", "/lib/title", () => {});
		expect(cdn.request.output).toBeNull();
		expect(cdn.request.options.output_dir).toBeNull();

		const ticket = argsFor("ctr-generate-ticket", "/lib/title", () => {});
		expect(ticket.request.output).toBe("/lib/title/ticket.tik");

		const nx = argsFor("nx-merge", "/lib/first.nsp", () => {});
		expect(nx.request.output).toBe("/lib/first (Merged).nsp");
	});

	it("splits a save-dialog pick into file name and folder", () => {
		const row = defByStoreId["ctr-cdn-to-cia"]!.outputRows.find((r) => r.kind === "save")!;
		const store = defByStoreId["ctr-cdn-to-cia"]!.useStore();
		row.set!(store, "/other/picked.cia");
		expect(store.output).toBe("picked.cia");
		expect(store.outputDir).toBe("/other");
	});

	it("keeps the derived base name when only a directory is picked", () => {
		const merge = argsFor("cue-merge", "/lib/game.cue", (s) => {
			s.outputDir = "/out";
		});
		expect(merge.request.output).toBe("/out/game (merged).cue");

		const cdn = argsFor("ctr-cdn-to-cia", "/lib/title", (s) => {
			s.outputDir = "/out";
		});
		expect(cdn.request.output).toBeNull();
		expect(cdn.request.options.output_dir).toBe("/out");

		const ticket = argsFor("ctr-generate-ticket", "/lib/title", (s) => {
			s.outputDir = "/out";
		});
		expect(ticket.request.output).toBe("/out/ticket.tik");

		const nx = argsFor("nx-merge", "/lib/first.nsp", (s) => {
			s.outputDir = "/out";
		});
		expect(nx.request.output).toBe("/out/first (Merged).nsp");
	});

	it("roots a saved file in its dialog folder", () => {
		const cdn = argsFor("ctr-cdn-to-cia", "/lib/title", (s) => {
			s.output = "picked.cia";
			s.outputDir = "/other";
		});
		expect(cdn.request.output).toBe("/other/picked.cia");
		expect(cdn.request.options.output_dir).toBeNull();

		const nx = argsFor("nx-merge", "/lib/first.nsp", (s) => {
			s.output = "picked.nsp";
			s.outputDir = "/other";
		});
		expect(nx.request.output).toBe("/other/picked.nsp");
	});

	it("re-roots a saved file when a directory is picked afterwards", () => {
		const merge = argsFor("cue-merge", "/lib/game.cue", (s) => {
			s.output = "picked.cue";
			s.outputDir = "/out";
		});
		expect(merge.request.output).toBe("/out/picked.cue");

		const ticket = argsFor("ctr-generate-ticket", "/lib/title", (s) => {
			s.output = "picked.tik";
			s.outputDir = "/out";
		});
		expect(ticket.request.output).toBe("/out/picked.tik");

		const nx = argsFor("nx-merge", "/lib/first.nsp", (s) => {
			s.output = "picked.nsp";
			s.outputDir = "/out";
		});
		expect(nx.request.output).toBe("/out/picked.nsp");
	});

	it("echoes the directory and file through the CLI flags", () => {
		const cdnDir = buildCliCommand(argsFor("ctr-cdn-to-cia", "/lib/title", (s) => {
			s.outputDir = "/out";
		}));
		expect(cdnDir).toContain("--output-dir /out");
		expect(cdnDir).not.toContain("picked");

		const ticketDir = buildCliCommand(argsFor("ctr-generate-ticket", "/lib/title", (s) => {
			s.outputDir = "/out";
		}));
		expect(ticketDir).toContain("/out/ticket.tik");

		const ticketFile = buildCliCommand(argsFor("ctr-generate-ticket", "/lib/title", (s) => {
			s.output = "picked.tik";
			s.outputDir = "";
		}));
		expect(ticketFile).toContain("/lib/title/picked.tik");

		const cdnFile = buildCliCommand(argsFor("ctr-cdn-to-cia", "/lib/title", (s) => {
			s.output = "picked.cia";
			s.outputDir = "/other";
		}));
		expect(cdnFile).toContain("/other/picked.cia");

		const nx = buildCliCommand(argsFor("nx-merge", "/lib/first.nsp", (s) => {
			s.outputDir = "/out";
		}));
		expect(nx).toContain("--output '/out/first (Merged).nsp'");

		const merge = buildCliCommand(argsFor("cue-merge", "/lib/game.cue", (s) => {
			s.outputDir = "/out";
		}));
		expect(merge).toContain("'/out/game (merged).cue'");
	});

	it("sends recursive cdn runs to the directory without an explicit file", () => {
		const dirOnly = argsFor("ctr-cdn-to-cia", "/lib/title", (s) => {
			s.recursive = true;
			s.outputDir = "/out";
		});
		expect(dirOnly.request.output).toBeNull();
		expect(dirOnly.request.options.output_dir).toBe("/out");
		const dirEcho = buildCliCommand(dirOnly);
		expect(dirEcho).toContain("--output-dir /out");
		expect(dirEcho).toContain("--recursive");

		const filePicked = argsFor("ctr-cdn-to-cia", "/lib/title", (s) => {
			s.recursive = true;
			s.output = "picked.cia";
			s.outputDir = "/other";
		});
		expect(filePicked.request.output).toBeNull();
		expect(filePicked.request.options.output_dir).toBe("/other");
		const fileRow = defByStoreId["ctr-cdn-to-cia"]!.outputRows.find((r) => r.kind === "save")!;
		expect(fileRow.display!(defByStoreId["ctr-cdn-to-cia"]!.useStore())).toBe("");
		const store = defByStoreId["ctr-cdn-to-cia"]!.useStore();
		store.recursive = false;
		expect(fileRow.display!(store)).toBe("picked.cia");
	});
});

describe("ctr bundle pages", () => {
	const items = [item("/roms/update.cia"), { ...item("/roms/game.cci"), id: "t2" }];

	function bundleArgsAll(mutate: (store: OpStore) => void) {
		const def = defByStoreId["ctr-bundle"]!;
		const store = def.useStore();
		mutate(store);
		return def.buildArgsAll!(store, items, "t1");
	}

	it("derives the name from the main member beside it by default", () => {
		const payload = bundleArgsAll(() => {});
		expect(payload.request.output).toBe("/roms/game.bcci");
		expect(payload.request.options.inputs).toEqual(["/roms/update.cia", "/roms/game.cci"]);
	});

	it("keeps the derived name when only a directory is picked", () => {
		expect(bundleArgsAll((s) => { s.outputDir = "/out"; }).request.output).toBe("/out/game.bcci");
	});

	it("roots a saved file in its dialog folder and re-roots it for a later directory", () => {
		const row = defByStoreId["ctr-bundle"]!.outputRows.find((r) => r.kind === "save")!;
		const store = defByStoreId["ctr-bundle"]!.useStore();
		row.set!(store, "/other/pack.bcci");
		expect(store.output).toBe("pack.bcci");
		expect(store.outputDir).toBe("/other");
		expect(defByStoreId["ctr-bundle"]!.buildArgsAll!(store, items, "t1").request.output).toBe("/other/pack.bcci");
		store.outputDir = "/out";
		expect(defByStoreId["ctr-bundle"]!.buildArgsAll!(store, items, "t1").request.output).toBe("/out/pack.bcci");
	});

	it("shows placeholders instead of fixed text so a chosen folder can be cleared", () => {
		for (const id of ["ctr-bundle", "ctr-unbundle"]) {
			const def = defByStoreId[id]!;
			const store = def.useStore();
			for (const row of def.outputRows) {
				expect(row.display(store), `${id}/${row.label}`).toBe("");
				expect(row.placeholder, `${id}/${row.label}`).toBeTruthy();
			}
		}
	});

	it("derives the unbundle folder beside an archived bundle and honours a chosen one", () => {
		const def = defByStoreId["ctr-unbundle"]!;
		expect(def.buildArgs(def.useStore(), item("/roms/game.bcci.zip"), "t1").request.output).toBe("/roms/game_unbundled");
		const store = def.useStore();
		store.outputDir = "/out";
		expect(def.buildArgs(store, item("/roms/game.bcci"), "t1").request.output).toBe("/out");
	});
});

