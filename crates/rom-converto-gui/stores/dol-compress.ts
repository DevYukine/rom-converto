import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useDolCompressStore = makeOpStore("dol-compress", () => ({
  input: "",
  output: "",
  level: 22,
  chunkSize: 131072,
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  verifyAfter: false,
  recursive: true,
  maxDepth: null as number | null,
}));
