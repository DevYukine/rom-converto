import { describe, expect, it } from "vitest";
import { rvzStructureOk } from "./fields";

describe("rvzStructureOk", () => {
	const base = { file_head_hash_ok: true, disc_hash_ok: true, part_hash_ok: true as boolean | null };

	it("passes when every checked hash passed, including an unchecked table", () => {
		expect(rvzStructureOk(base)).toBe(true);
		expect(rvzStructureOk({ ...base, part_hash_ok: null })).toBe(true);
	});

	it("fails on any failed hash", () => {
		expect(rvzStructureOk({ ...base, part_hash_ok: false })).toBe(false);
		expect(rvzStructureOk({ ...base, disc_hash_ok: false })).toBe(false);
		expect(rvzStructureOk({ ...base, file_head_hash_ok: false })).toBe(false);
	});
});
