// RvzStructuralVerify serializes no verdict field: the structure is ok when
// every stored hash that was checked passed.
export function rvzStructureOk(s: {
	file_head_hash_ok: boolean;
	disc_hash_ok: boolean;
	part_hash_ok: boolean | null;
}): boolean {
	return s.file_head_hash_ok && s.disc_hash_ok && s.part_hash_ok !== false;
}
