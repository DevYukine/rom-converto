import { beforeEach, describe, expect, it } from "vitest";
import { createPinia, setActivePinia } from "pinia";
import { allOpDefs } from "./index";
import { runArgs, withSourceRoot, type StagedItem } from "./types";

const ITEM: StagedItem = { id: "1", path: "/roms/a/g.iso", name: "g.iso", size: 0, outExt: "chd", sourceRoot: "/roms" };
const defs = allOpDefs();
const payload = (options: Record<string, unknown>) => runArgs("x", null, null, options, false, "t");
const sent = (storeId: string, set: Record<string, unknown>) => {
	const def = defs.find((d) => d.storeId === storeId)!;
	const store = def.useStore();
	Object.assign(store, { deleteSource: true, ...set });
	const field = def.fields.find((f) => f.key === "deleteSource");
	const disabled = field?.kind === "toggle" && !!field.disabled?.(store);
	return { options: def.buildArgs(store, ITEM, "t").request.options, disabled };
};

describe("delete-source gating", () => {
	beforeEach(() => setActivePinia(createPinia()));

	it("withSourceRoot adds source_root only with delete_source and a scan root", () => {
		const on = withSourceRoot(payload({ delete_source: true }), ITEM);
		expect(on.request.options.source_root).toBe("/roms");
		const off = withSourceRoot(payload({}), ITEM);
		expect(off.request.options.source_root).toBeUndefined();
		expect(withSourceRoot(payload({ delete_source: true }), { ...ITEM, sourceRoot: undefined }).request.options.source_root).toBeUndefined();
	});

	it.each(["xbox-convert", "wup-convert", "xenon-compress"])("%s drops delete while Compute output hash is on", (id) => {
		const a = sent(id, { verifyAfter: true });
		expect(a.disabled).toBe(true);
		expect(a.options.delete_source).toBe(false);
		expect(sent(id, { verifyAfter: false }).options.delete_source).toBe(true);
	});

	it("keeps delete usable with Verify after conversion on", () => {
		const a = sent("chd-compress", { verifyAfter: true });
		expect(a.disabled).toBe(false);
		expect(a.options.delete_source).toBe(true);
	});
});
