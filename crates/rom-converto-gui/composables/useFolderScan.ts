import { invoke } from "~/lib/ipc";

export function useFolderScan(exts: string[]) {
  // Returns null when the path is not a scannable directory. A successful
  // scan is authoritative even when it finds nothing, so callers can tell an
  // empty directory apart from a plain file.
  async function expand(path: string, maxDepth: number | null): Promise<string[] | null> {
    try {
      return await invoke<string[]>("cmd_scan_dir", {
        dir: path,
        exts,
        maxDepth,
      });
    } catch {
      return null;
    }
  }
  return { expand };
}
