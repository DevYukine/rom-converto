import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useCueMergeStore = makeOpStore("cue-merge", () => ({
  input: "",
  output: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
}));
