export interface Bounded {
	min?: number;
	max?: number;
}

// Rounds and clamps a number input to the field's inclusive bounds, and
// reflects the stored value back into the input so typing 27 over a stored
// 26 does not leave 27 on screen while 26 travels to the runner.
export function boundedNumber(e: Event, field: Bounded): number | null {
	const raw = (e.target as HTMLInputElement).value;
	if (raw === "") return null;
	const value = Math.round(Number(raw));
	if (!Number.isFinite(value)) return null;
	const min = field.min ?? -Infinity;
	const max = field.max ?? Infinity;
	const clamped = Math.min(Math.max(value, min), max);
	(e.target as HTMLInputElement).value = String(clamped);
	return clamped;
}

// RvzStructuralVerify serializes no verdict field: the structure is ok when
// every stored hash that was checked passed.
export function rvzStructureOk(s: {
	file_head_hash_ok: boolean;
	disc_hash_ok: boolean;
	part_hash_ok: boolean | null;
}): boolean {
	return s.file_head_hash_ok && s.disc_hash_ok && s.part_hash_ok !== false;
}
