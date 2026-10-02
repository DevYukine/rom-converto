import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useChdExtractStore = makeOpStore("chd-extract", () => ({
  input: "",
  output: "",
  onConflict: useUiStore().defaultOnConflict,
  parent: "",
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  recursive: true,
  maxDepth: null as number | null,
}));
