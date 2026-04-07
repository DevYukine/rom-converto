const COMPRESS_MAP: Record<string, string> = {
  cia: "zcia",
  cci: "zcci",
  "3ds": "zcci",
  cxi: "zcxi",
  "3dsx": "z3dsx",
};

const DECOMPRESS_MAP: Record<string, string> = {
  zcia: "cia",
  zcci: "cci",
  zcxi: "cxi",
  z3dsx: "3dsx",
};

function replaceExt(path: string, newExt: string): string {
  const dot = path.lastIndexOf(".");
  if (dot === -1) return `${path}.${newExt}`;
  return `${path.slice(0, dot)}.${newExt}`;
}

function getExt(path: string): string {
  const dot = path.lastIndexOf(".");
  if (dot === -1) return "";
  return path.slice(dot + 1).toLowerCase();
}

export function deriveCompressedPath(input: string): string {
  const ext = getExt(input);
  return replaceExt(input, COMPRESS_MAP[ext] ?? "z3ds");
}

export function deriveDecompressedPath(input: string): string {
  const ext = getExt(input);
  return replaceExt(input, DECOMPRESS_MAP[ext] ?? "3ds");
}

export function deriveDecryptedPath(input: string): string {
  const ext = getExt(input);
  const dot = input.lastIndexOf(".");
  const stem = dot === -1 ? input : input.slice(0, dot);
  return `${stem}.decrypted.${ext || "cia"}`;
}

export function deriveChdPath(input: string): string {
  return replaceExt(input, "chd");
}

export function deriveCuePath(input: string): string {
  return replaceExt(input, "cue");
}
