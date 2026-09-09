import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useNtrDecryptStore = makeOpStore("ntr-decrypt", () => ({
  input: "",
  output: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  recursive: true,
  maxDepth: null as number | null,
}));
