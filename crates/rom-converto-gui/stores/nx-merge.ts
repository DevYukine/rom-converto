import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useNxMergeStore = makeOpStore("nx-merge", () => ({
  output: "",
  outputDir: "",
  keys: "",
  format: "nsp",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
}));
