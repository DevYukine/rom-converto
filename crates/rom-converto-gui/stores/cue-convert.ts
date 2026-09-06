import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useCueConvertStore = makeOpStore("cue-convert", () => ({
  input: "",
  format: "zso" as "iso" | "cso" | "zso",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  recursive: true,
  maxDepth: null as number | null,
}));
