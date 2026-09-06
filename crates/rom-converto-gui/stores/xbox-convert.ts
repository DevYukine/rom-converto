import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useXboxConvertStore = makeOpStore("xbox-convert", () => ({
  input: "",
  output: "",
  mediaPatch: true,
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  verifyAfter: false,
  recursive: true,
  maxDepth: null as number | null,
}));
