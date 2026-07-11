// Parses cmd_hash output rows: `path  algo=hex  algo=hex`.

export interface HashLine {
	path: string;
	values: { label: string; value: string }[];
}

const ALGO_LABELS: Record<string, string> = {
	crc32: "CRC32",
	md5: "MD5",
	sha1: "SHA-1",
	sha256: "SHA-256",
};
const ALGO_ORDER = ["crc32", "md5", "sha1", "sha256"];

export function parseHashLine(line: string): HashLine | null {
	const cells = line.split("  ").filter((c) => c.length > 0);
	if (cells.length === 0) return null;
	const path = cells[0]!;
	const found: Record<string, string> = {};
	for (const cell of cells.slice(1)) {
		const eq = cell.indexOf("=");
		if (eq === -1) continue;
		found[cell.slice(0, eq)] = cell.slice(eq + 1);
	}
	const values = ALGO_ORDER.filter((a) => found[a]).map((a) => ({ label: ALGO_LABELS[a]!, value: found[a]! }));
	return { path, values };
}
