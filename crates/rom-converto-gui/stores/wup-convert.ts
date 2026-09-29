import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useWupConvertStore = makeOpStore("wup-convert", () => ({
  input: "",
  output: "",
  direction: "wux" as "wux" | "wud",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  verifyAfter: false,
  recursive: true,
  maxDepth: null as number | null,
}));
