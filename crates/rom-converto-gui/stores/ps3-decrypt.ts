import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const usePs3DecryptStore = makeOpStore("ps3-decrypt", () => ({
  input: "",
  output: "",
  key: "",
  skipProbe: false,
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  recursive: true,
  maxDepth: null as number | null,
}));
