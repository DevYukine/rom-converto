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

export function deriveRvzPath(input: string): string {
  return replaceExt(input, "rvz");
}

export function deriveDiscIsoPath(input: string): string {
  return replaceExt(input, "iso");
}

/// Derive a `.wua` output path from a title input. Strips a disc
/// image extension, then peels trailing parenthesised tags in the
/// preservation naming style so `Title (Region) (Langs) (Update)`
/// collapses to `Title`. Dots inside the title (`Super Mario Bros. U`)
/// are preserved; only real `.wud` / `.wux` extensions are stripped.
export function deriveWuaPath(input: string): string {
  const trimmed = input.replace(/[\\/]+$/, "");
  const slash = Math.max(trimmed.lastIndexOf("/"), trimmed.lastIndexOf("\\"));
  const parent = slash >= 0 ? trimmed.slice(0, slash) : "";
  let base = slash >= 0 ? trimmed.slice(slash + 1) : trimmed;

  base = base.replace(/\.(wud|wux)$/i, "");

  while (true) {
    const next = base.replace(/\s*\([^()]*\)\s*$/, "");
    if (next === base) break;
    base = next;
  }
  base = base.trimEnd();

  const sep = input.includes("\\") ? "\\" : "/";
  return parent ? `${parent}${sep}${base}.wua` : `${base}.wua`;
}
