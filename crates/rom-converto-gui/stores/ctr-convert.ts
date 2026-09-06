import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useCtrConvertStore = makeOpStore("ctr-convert", () => ({
  input: "",
  output: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  verifyAfter: false,
  recursive: true,
  maxDepth: null as number | null,
}));
