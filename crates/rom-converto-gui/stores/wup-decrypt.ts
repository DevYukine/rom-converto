import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useWupDecryptStore = makeOpStore("wup-decrypt", () => ({
  input: "",
  output: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
}));
