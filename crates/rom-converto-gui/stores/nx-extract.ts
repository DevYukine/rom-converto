import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useNxExtractStore = makeOpStore("nx-extract", () => ({
  keys: "",
  outputDir: "",
  romfs: true,
  exefs: true,
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
}));
