import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useXenonConvertStore = makeOpStore("xenon-convert", () => ({
  input: "",
  outputDir: "",
  title: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  recursive: true,
  maxDepth: null as number | null,
}));
