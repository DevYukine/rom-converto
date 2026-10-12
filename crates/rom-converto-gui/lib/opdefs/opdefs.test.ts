import { beforeEach, describe, expect, it } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { registeredCommands, runOptionKeys } from "../source-parsers";
import { buildCliCommand, setWindowsQuoting } from "~/composables/useCliEcho";
import { allOpDefs, opCommand } from "./index";
import { pageEchoArgs, type OpStore, type RunPayload, type StagedItem } from "./types";

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
	beforeEach(() => setActivePinia(createPinia()));

	it("registers every op module", () => {
		expect(defs.length).toBeGreaterThan(0);
	});

	it("lets CHD extraction choose its output format beside the input", () => {
		const def = defs.find((def) => def.storeId === "chd-extract")!;
		const item = { ...ITEM, path: "/roms/Game (USA).chd" };
		const { request } = def.buildArgs(def.useStore(), item, "task-1");
		expect(request.output).toBeNull();
		expect(request.options.output_dir).toBe("/roms");
		// A template leaves the name to the runner, but the input folder must
		// still reach the echo as --output-dir.
		const store = def.useStore();
		store.outputTemplate = "{basename}.cue";
		const templated = def.buildArgs(store, item, "task-1");
		expect(templated.request.options.output_dir).toBe("/roms");
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

describe("nx decrypt page args", () => {
	// Each test mutates the shared op store, so every one starts from defaults.
	beforeEach(() => setActivePinia(createPinia()));

	function requestOf(path: string, mutate: (store: OpStore) => void = () => {}) {
		const def = defs.find((def) => def.storeId === "nx-decrypt")!;
		const store = def.useStore();
		mutate(store);
		return def.buildArgs(store, { ...ITEM, path }, "task-1").request;
	}

	it("lets the runner name archive outputs inside the chosen directory", () => {
		const plain = requestOf("/roms/Game (USA).zip");
		expect(plain.output).toBeNull();
		expect(plain.options.output_dir).toBeNull();

		const directed = requestOf("/roms/Game (USA).zip", (store) => { store.outputDir = "/out"; });
		expect(directed.output).toBeNull();
		expect(directed.options.output_dir).toBe("/out");
	});

	it("lets the runner place templated outputs in the directory", () => {
		const request = requestOf("/roms/Game (USA).nsp", (store) => {
			store.outputTemplate = "{title}.{ext}";
			store.outputDir = "/out";
		});
		expect(request.output).toBeNull();
		expect(request.options.output_dir).toBe("/out");
	});

	it("keeps the explicit derived output for plain inputs", () => {
		expect(requestOf("/roms/Game (USA).nsp").output).toBe("/roms/Game (USA).dnsp");
		expect(requestOf("/roms/Game (USA).xci").output).toBe("/roms/Game (USA).dxci");

		const directed = requestOf("/roms/Game (USA).xci", (store) => { store.outputDir = "/out"; });
		expect(directed.output).toBe("/out/Game (USA).dxci");
		expect(directed.options.output_dir ?? null).toBeNull();
	});
});

describe("archive outputs on runner-derived pages", () => {
	// Each case mutates the shared op store, so every case starts fresh.
	const cases: Array<[string, string, string]> = [
		["ctr-decrypt", "/roms/Game (USA).3ds", "/roms/Game (USA).decrypted.3ds"],
		["ntr-decrypt", "/roms/Game (USA).nds", "/roms/Game (USA).decrypted.nds"],
		["ctr-encrypt", "/roms/Game (USA).3ds", "/roms/Game (USA).encrypted.3ds"],
		["ntr-encrypt", "/roms/Game (USA).nds", "/roms/Game (USA).encrypted.nds"],
		["ctr-compress", "/roms/Game (USA).3ds", "/roms/Game (USA).zcci"],
		["ctr-convert", "/roms/Game (USA).cia", "/roms/Game (USA).3ds"],
		["nx-compress", "/roms/Game (USA).nsp", "/roms/Game (USA).nsz"],
		["nx-decompress", "/roms/Game (USA).nsz", "/roms/Game (USA).nsp"],
		["ctr-decompress", "/roms/Game (USA).zcia", "/roms/Game (USA).cia"],
	];

	it.each(cases)("lets the runner name %s archive outputs inside the chosen directory", (storeId, plainPath, plainOutput) => {
		setActivePinia(createPinia());
		const def = defs.find((def) => def.storeId === storeId)!;

		// Plain inputs keep their derived path and no directory option.
		const plain = def.buildArgs(def.useStore(), { ...ITEM, path: plainPath }, "task-1").request;
		expect(plain.output, storeId).toBe(plainOutput);
		expect(plain.options.output_dir ?? null, storeId).toBeNull();

		// An archive leaves the name to the runner; output_dir carries the
		// chosen directory, or null so the runner defaults beside the input.
		const archive = { ...ITEM, path: "/roms/Game (USA).zip" };
		const bare = def.buildArgs(def.useStore(), archive, "task-1").request;
		expect(bare.output, storeId).toBeNull();
		expect(bare.options.output_dir ?? null, storeId).toBeNull();

		def.useStore().outputDir = "/out";
		const directed = def.buildArgs(def.useStore(), archive, "task-1").request;
		expect(directed.output, storeId).toBeNull();
		expect(directed.options.output_dir, storeId).toBe("/out");
	});
});

describe("templated jobs carry the output directory", () => {
	beforeEach(() => setActivePinia(createPinia()));

	// cso-to-chd nulls its output under a template and adds nothing else, so
	// the directory can only arrive through commonOptions.
	function csoToChdRequest(mutate: (store: OpStore) => void) {
		const def = defs.find((def) => def.storeId === "cso-to-chd")!;
		const store = def.useStore();
		mutate(store);
		return def.buildArgs(store, { ...ITEM, path: "/roms/sample.cso" }, "task-1").request;
	}

	it("sends the chosen directory for a templated output", () => {
		const directed = csoToChdRequest((store) => {
			store.outputTemplate = "{title}.{ext}";
			store.outputDir = "/out";
		});
		expect(directed.output).toBeNull();
		expect(directed.options.output_dir).toBe("/out");
	});

	it("sends a null directory for a templated output without one", () => {
		const bare = csoToChdRequest((store) => {
			store.outputTemplate = "{title}.{ext}";
		});
		expect(bare.output).toBeNull();
		expect(bare.options.output_dir ?? null).toBeNull();
	});
});

describe("the PSP / PS2 convert page routes by extension", () => {
	beforeEach(() => setActivePinia(createPinia()));

	const cases: [string, string, string, string][] = [
		["/roms/game.iso", "chd.compress", "/roms/game.chd", "rom-converto chd compress"],
		["/roms/GAME.ISO", "chd.compress", "/roms/GAME.chd", "rom-converto chd compress"],
		["/roms/game.cso", "cso.to_chd", "/roms/game.chd", "rom-converto cso to-chd"],
		["/roms/game.zso", "cso.to_chd", "/roms/game.chd", "rom-converto cso to-chd"],
		["/roms/game.dax", "cso.to_chd", "/roms/game.chd", "rom-converto cso to-chd"],
		["/roms/game.zip", "cso.to_chd", "/roms/game.chd", "rom-converto cso to-chd"],
		["/roms/game.iso.zip", "cso.to_chd", "/roms/game.chd", "rom-converto cso to-chd"],
	];

	it.each(cases)("runs %s through %s", (path, operation, output, echo) => {
		const def = defs.find((def) => def.storeId === "cso-to-chd")!;
		const payload = def.buildArgs(def.useStore(), { ...ITEM, path }, "task-1");
		expect(payload.request.operation).toBe(operation);
		expect(payload.request.output).toBe(output);
		expect(buildCliCommand(payload)).toMatch(new RegExp(`^${echo}\\b`));
	});
});

describe("disc convert page args", () => {
	beforeEach(() => {
		setActivePinia(createPinia());
		setWindowsQuoting(false);
	});

	it.each(["dol", "rvl"])("defaults %s to ISO without sending unset options", (console) => {
		const def = defs.find((def) => def.storeId === `${console}-convert`)!;
		const store = def.useStore();
		expect(store.format).toBe("iso");
		expect(store.level).toBeNull();
		expect(store.skipVerify).toBe(false);
		const payload = def.buildArgs(store, { ...ITEM, path: "/roms/game.nkit.gcz" }, "task-1");
		expect(payload.request.operation).toBe(`${console}.convert`);
		expect(payload.request.output).toBe("/roms/game.iso");
		expect(payload.request.options.format).toBe("iso");
		for (const key of ["level", "chunk_size", "block_size", "compression", "skip_verify"]) {
			expect(payload.request.options).not.toHaveProperty(key);
		}
	});

	it.each([
		["dol", "iso"], ["dol", "gcz"], ["dol", "rvz"],
		["rvl", "iso"], ["rvl", "wbfs"], ["rvl", "gcz"], ["rvl", "wia"], ["rvl", "rvz"],
	])("derives and echoes %s conversion to %s", (console, format) => {
		const def = defs.find((def) => def.storeId === `${console}-convert`)!;
		const store = def.useStore();
		store.format = format;
		const payload = def.buildArgs(store, { ...ITEM, path: "/roms/game.nkit.iso" }, "task-1");
		expect(payload.request.output).toBe(`/roms/game.${format}`);
		expect(payload.request.options.format).toBe(format);
		const echo = buildCliCommand(payload);
		expect(echo).toMatch(new RegExp(`^rom-converto ${console} convert\\b`));
		expect(echo).toContain(`--format ${format}`);
		expect(echo).not.toContain("--level");
	});

	it.each([["dol", "rvz", 22], ["rvl", "rvz", 10], ["rvl", "wia", 0]])(
		"sends a typed level for %s %s",
		(console, format, level) => {
			const def = defs.find((def) => def.storeId === `${console}-convert`)!;
			const store = def.useStore();
			store.format = format;
			expect(def.buildArgs(store, ITEM, "task-1").request.options).not.toHaveProperty("level");
			store.level = level;
			const payload = def.buildArgs(store, ITEM, "task-1");
			expect(payload.request.options.level).toBe(level);
			expect(buildCliCommand(payload)).toContain(`--level ${level}`);
			store.level = null;
			expect(def.buildArgs(store, ITEM, "task-1").request.options).not.toHaveProperty("level");
		},
	);

	it("only sends compression settings that apply to the selected format", () => {
		const def = defs.find((def) => def.storeId === "rvl-convert")!;
		const store = def.useStore();
		store.compression = "lzma2";
		store.level = 6;
		store.chunkSize = 262144;
		store.blockSize = 131072;
		store.skipVerify = true;
		for (const format of ["iso", "wbfs", "gcz", "wia", "rvz"]) {
			store.format = format;
			const payload = def.buildArgs(store, ITEM, "task-1");
			const options = payload.request.options;
			expect("level" in options).toBe(format === "rvz" || format === "wia");
			expect("chunk_size" in options).toBe(format === "rvz");
			expect("block_size" in options).toBe(format === "gcz");
			expect("compression" in options).toBe(format === "wia");
			if (format === "wia") {
				expect(options.compression).toBe("lzma2");
				expect(buildCliCommand(payload)).toContain("--compression lzma2");
			}
			expect(options.skip_verify).toBe(true);
			expect(buildCliCommand(payload)).toContain("--skip-verify");
		}
	});

	it.each([
		["dol", ["iso", "gcm", "rvz", "gcz"]],
		["rvl", ["iso", "wbfs", "rvz", "gcz", "wia"]],
	] as const)("accepts %s disc images and archives", (console, images) => {
		const def = defs.find((def) => def.storeId === `${console}-convert`)!;
		const archives = ["zip", "7z", "rar", "tar", "tgz", "gz"];
		expect(def.acceptedExts).toHaveLength(images.length + archives.length);
		expect(def.acceptedExts).toEqual(expect.arrayContaining([...images, ...archives]));
	});

	it("omits unset WIA compression, including after clearing a selection", () => {
		const def = defs.find((def) => def.storeId === "rvl-convert")!;
		const store = def.useStore();
		store.format = "wia";
		expect(store.compression).toBeNull();
		expect(def.buildArgs(store, ITEM, "task-1").request.options).not.toHaveProperty("compression");
		store.compression = "bzip2";
		expect(def.buildArgs(store, ITEM, "task-1").request.options.compression).toBe("bzip2");
		const field = def.fields.find((field) => field.key === "compression")!;
		expect(field.kind).toBe("select");
		if (field.kind !== "select") throw new Error("Expected a select field");
		store.compression = field.options.find((option) => option.value === "")!.value;
		expect(def.buildArgs(store, ITEM, "task-1").request.options).not.toHaveProperty("compression");
	});

	it.each([
		["dol", "rvz", "gcz"],
		["rvl", "rvz", "wia"],
		["rvl", "wia", "rvz"],
	])("clears the level when changing %s from %s to %s", (console, from, to) => {
		const def = defs.find((def) => def.storeId === `${console}-convert`)!;
		const store = def.useStore();
		store.format = from;
		store.level = 22;
		const field = def.fields.find((field) => field.key === "format")!;
		expect(field.kind).toBe("segmented");
		if (field.kind !== "segmented") throw new Error("Expected a segmented field");
		store.format = to;
		field.onSet?.(store);
		expect(store.level).toBeNull();
		expect(def.buildArgs(store, ITEM, "task-1").request.options).not.toHaveProperty("level");
	});

	it("keeps output templates and directories on the runner-derived path", () => {
		const def = defs.find((def) => def.storeId === "rvl-convert")!;
		const store = def.useStore();
		store.format = "wia";
		store.outputDir = "/out";
		store.outputTemplate = "{title}.{ext}";
		const request = def.buildArgs(store, ITEM, "task-1").request;
		expect(request.output).toBeNull();
		expect(request.options.output_dir).toBe("/out");
		expect(request.options.output_template).toBe("{title}.{ext}");
	});
});

describe("disc extract verification options", () => {
	beforeEach(() => {
		setActivePinia(createPinia());
		setWindowsQuoting(false);
	});

	it.each(["dol", "rvl"])("only sends skip_verify for %s when enabled", (console) => {
		const def = defs.find((def) => def.storeId === `${console}-decompress`)!;
		const store = def.useStore();
		const field = def.fields.find((field) => field.key === "skipVerify")!;
		expect(field.kind).toBe("toggle");
		expect(def.buildArgs(store, ITEM, "task-1").request.options).not.toHaveProperty("skip_verify");
		store[field.key] = true;
		const payload = def.buildArgs(store, ITEM, "task-1");
		expect(payload.request.options.skip_verify).toBe(true);
		expect(buildCliCommand(payload)).toContain("--skip-verify");
		store[field.key] = false;
		expect(def.buildArgs(store, ITEM, "task-1").request.options).not.toHaveProperty("skip_verify");
	});
});
