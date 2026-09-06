import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useNxDecompressStore = makeOpStore("nx-decompress", () => ({
  recursive: true,
  maxDepth: null as number | null,
  output: "",
  keys: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
}));
