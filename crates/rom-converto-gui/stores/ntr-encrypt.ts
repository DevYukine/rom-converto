import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useNtrEncryptStore = makeOpStore("ntr-encrypt", () => ({
  input: "",
  output: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  recursive: true,
  maxDepth: null as number | null,
}));
