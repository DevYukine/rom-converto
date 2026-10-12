import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useDolConvertStore = makeOpStore("dol-convert", () => ({
  input: "",
  output: "",
  outputDir: "",
  format: "iso" as "iso" | "gcz" | "rvz",
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
