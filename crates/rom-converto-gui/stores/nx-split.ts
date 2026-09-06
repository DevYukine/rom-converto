import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useNxSplitStore = makeOpStore("nx-split", () => ({
  keys: "",
  outputDir: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
}));
