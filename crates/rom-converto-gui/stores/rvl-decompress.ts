import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useRvlDecompressStore = makeOpStore("rvl-decompress", () => ({
  input: "",
  output: "",
  format: "iso" as "iso" | "wbfs",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  recursive: true,
  maxDepth: null as number | null,
}));
