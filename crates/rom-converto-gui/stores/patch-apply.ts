import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const usePatchApplyStore = makeOpStore("patch-apply", () => ({
  input: "",
  output: "",
  outputDir: "",
  outputTemplate: "",
  patchFile: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
}));
