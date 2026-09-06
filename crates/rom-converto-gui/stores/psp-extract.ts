import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const usePspExtractStore = makeOpStore("psp-extract", () => ({
  input: "",
  outputDir: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  recursive: true,
  maxDepth: null as number | null,
}));
