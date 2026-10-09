import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useCtrBundleStore = makeOpStore("ctr-bundle", () => ({
  output: "",
  outputDir: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
}));
