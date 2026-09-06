import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useDolDecompressStore = makeOpStore("dol-decompress", () => ({
  input: "",
  output: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  recursive: true,
  maxDepth: null as number | null,
}));
