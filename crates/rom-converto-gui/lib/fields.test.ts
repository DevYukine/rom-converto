import { describe, expect, it } from "vitest";
import { boundedNumber, rvzStructureOk } from "./fields";

function inputEvent(value: string): Event {
	return { target: { value } } as unknown as Event;
}

describe("boundedNumber", () => {
	const BOUNDS = { min: 1, max: 26 };

	it("clamps to the field's inclusive bounds and writes the value back", () => {
		const low = inputEvent("0");
		expect(boundedNumber(low, BOUNDS)).toBe(1);
		expect((low.target as HTMLInputElement).value).toBe("1");
		const high = inputEvent("267");
		expect(boundedNumber(high, BOUNDS)).toBe(26);
		expect((high.target as HTMLInputElement).value).toBe("26");
	});

	it("rounds fractions to integers", () => {
		expect(boundedNumber(inputEvent("1.5"), BOUNDS)).toBe(2);
	});

	it("passes in-range values through and keeps blank unset", () => {
		expect(boundedNumber(inputEvent("7"), BOUNDS)).toBe(7);
		expect(boundedNumber(inputEvent(""), BOUNDS)).toBeNull();
		expect(boundedNumber(inputEvent("abc"), BOUNDS)).toBeNull();
		expect(boundedNumber(inputEvent("40"), {})).toBe(40);
	});
});

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
