import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

/// True when the input path ends in `.wud` or `.wux`, so the UI
/// can offer a manual master key override. Mirrors the Rust extension check.
export function isDiscInput(input: string): boolean {
  const lower = input.toLowerCase();
  return lower.endsWith(".wud") || lower.endsWith(".wux");
}

export const useWupCompressStore = makeOpStore("wup-compress", () => ({
  output: "",
  // Zstd level: 0 = Cemu default (6), 1..22 = explicit.
  level: 0,
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
}));
