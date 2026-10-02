import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useNxDecryptStore = makeOpStore("nx-decrypt", () => ({
  recursive: true,
  maxDepth: null as number | null,
  output: "",
  keys: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
}));
