import { invoke } from "~/lib/ipc";

export function useFolderScan(exts: string[]) {
  async function expand(path: string, maxDepth: number | null): Promise<string[]> {
    try {
      const found = await invoke<string[]>("cmd_scan_dir", {
        dir: path,
        exts,
        maxDepth,
      });
      return found.length > 0 ? found : [path];
    } catch {
      return [path];
    }
  }
  return { expand };
}
