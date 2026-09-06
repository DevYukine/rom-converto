import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useXenonCompressStore = makeOpStore("xenon-compress", () => ({
  input: "",
  output: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  verifyAfter: false,
  recursive: true,
  maxDepth: null as number | null,
}));
