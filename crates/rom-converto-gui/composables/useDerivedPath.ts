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

// Extensions the ops read or write. Once an archive wrapper is stripped the
// remaining name has no reliable extension, so only a trailing token from
// this list is treated as one; anything else stays part of the title
// ("10.000 Bullets (Europe)", "Super Mario Bros. U").
const IMAGE_EXTS = new Set([
  "3ds", "3dsx", "bin", "cci", "chd", "cia", "cso", "cue", "cxi", "dax",
  "gcm", "gcz", "iso", "nsp", "nsz", "rvz", "wbfs", "wia", "wua", "wud",
  "wux", "xci", "xcz", "z3ds", "z3dsx", "zcia", "zcci", "zcxi", "zso",
]);

function getExt(path: string): string {
  const dot = path.lastIndexOf(".");
  if (dot === -1) return "";
  return path.slice(dot + 1).toLowerCase();
}

function knownExt(path: string): string {
  const ext = getExt(path);
  return IMAGE_EXTS.has(ext) ? ext : "";
}

function stemOf(path: string): string {
  if (knownExt(path)) return path.slice(0, path.lastIndexOf("."));
  return path.replace(/\.+$/, "");
}

function replaceExt(path: string, newExt: string): string {
  return `${stemOf(path)}.${newExt}`;
}

const ARCHIVE_EXTS = ["zip", "7z", "rar", "tar", "tgz", "gz"];

// Archive wrappers are transparent inputs: the backend extracts the inner
// image, so a picked `game.zip` should derive output names as if it were
// `game`. A `.tar.gz` collapses both extensions.
export function stripArchiveExt(name: string): string {
  if (/\.tar\.gz$/i.test(name)) return name.slice(0, -7);
  if (!ARCHIVE_EXTS.includes(getExt(name))) return name;
  const dot = name.lastIndexOf(".");
  return dot === -1 ? name : name.slice(0, dot);
}

export function basename(path: string): string {
  const norm = path.replace(/[\\/]+$/, "");
  const i = Math.max(norm.lastIndexOf("/"), norm.lastIndexOf("\\"));
  return i >= 0 ? norm.slice(i + 1) : norm;
}

// Splits a path's filename into a shrinkable head and a fixed tail for
// middle-ellipsis display. The tail keeps the extension and trailing
// disambiguating tags (region, version) which are as identifying as the start.
export function splitFilenameForDisplay(path: string, tailLen = 12): [string, string] {
  const name = basename(path);
  if (name.length <= tailLen) return ["", name];
  return [name.slice(0, name.length - tailLen), name.slice(name.length - tailLen)];
}

// An empty `dir` is a deliberate no-op so the output stays next to its input,
// preserving the existing default for pages where no directory is chosen.
export function withOutputDir(derivedPath: string, dir: string): string {
  if (!dir) return derivedPath;
  const sep = dir.includes("\\") || derivedPath.includes("\\") ? "\\" : "/";
  const cleanDir = dir.replace(/[\\/]+$/, "");
  return `${cleanDir}${sep}${basename(derivedPath)}`;
}

export function deriveCompressedPath(input: string): string {
  input = stripArchiveExt(input);
  const ext = getExt(input);
  return replaceExt(input, COMPRESS_MAP[ext] ?? "z3ds");
}

export function deriveDecompressedPath(input: string): string {
  input = stripArchiveExt(input);
  const ext = getExt(input);
  return replaceExt(input, DECOMPRESS_MAP[ext] ?? "3ds");
}

export function deriveDecryptedPath(input: string): string {
  input = stripArchiveExt(input);
  return `${stemOf(input)}.decrypted.${knownExt(input) || "cia"}`;
}

export function deriveEncryptedPath(input: string): string {
  input = stripArchiveExt(input);
  return `${stemOf(input)}.encrypted.${knownExt(input) || "cia"}`;
}

const CONVERT_MAP: Record<string, string> = {
  cia: "3ds",
  "3ds": "cia",
  cci: "cia",
};

export function deriveConvertedPath(input: string): string {
  input = stripArchiveExt(input);
  const ext = getExt(input);
  return replaceExt(input, CONVERT_MAP[ext] ?? "out");
}

export function deriveChdPath(input: string): string {
  return replaceExt(stripArchiveExt(input), "chd");
}

export function deriveCuePath(input: string): string {
  return replaceExt(stripArchiveExt(input), "cue");
}

// Strips a trailing "(Track N)" tag so the merged output never collides
// with one of the input bin files.
export function deriveMergedCuePath(input: string): string {
  input = stripArchiveExt(input);
  const stem = stemOf(input).replace(/\s*\(Track\s*\d+\)\s*$/i, "");
  return `${stem} (merged).cue`;
}

export function deriveCsoPath(input: string, format: "cso" | "zso"): string {
  return replaceExt(stripArchiveExt(input), format);
}

export function deriveRvzPath(input: string): string {
  input = stripArchiveExt(input);
  // NKit double extensions collapse: game.nkit.iso -> game.rvz.
  const lower = input.toLowerCase();
  if (lower.endsWith(".nkit.iso") || lower.endsWith(".nkit.gcz")) {
    return `${input.slice(0, -9)}.rvz`;
  }
  return replaceExt(input, "rvz");
}

export function deriveDiscIsoPath(input: string): string {
  return replaceExt(stripArchiveExt(input), "iso");
}

export function deriveDiscPath(input: string, format: "iso" | "wbfs"): string {
  return replaceExt(stripArchiveExt(input), format);
}

export function deriveNszPath(input: string): string {
  input = stripArchiveExt(input);
  const ext = getExt(input);
  if (ext === "xci") return replaceExt(input, "xcz");
  return replaceExt(input, "nsz");
}

export function deriveNspPath(input: string): string {
  input = stripArchiveExt(input);
  const ext = getExt(input);
  if (ext === "xcz") return replaceExt(input, "xci");
  return replaceExt(input, "nsp");
}

/// Derive a `.wua` output path from a title input. Strips a disc
/// image extension, then peels trailing parenthesised tags in the
/// preservation naming style so `Title (Region) (Langs) (Update)`
/// collapses to `Title`. Dots inside the title (`Super Mario Bros. U`)
/// are preserved; only real `.wud` / `.wux` extensions are stripped.
export function deriveWuaPath(input: string): string {
  input = stripArchiveExt(input);
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
