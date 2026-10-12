import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useRvlConvertStore = makeOpStore("rvl-convert", () => ({
  input: "",
  output: "",
  outputDir: "",
  format: "iso" as "iso" | "wbfs" | "gcz" | "wia" | "rvz",
  compression: null as "" | "none" | "bzip2" | "lzma" | "lzma2" | null,
  level: null as number | null,
  chunkSize: null as number | null,
  blockSize: null as number | null,
  skipVerify: false,
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  recursive: true,
  maxDepth: null as number | null,
}));
