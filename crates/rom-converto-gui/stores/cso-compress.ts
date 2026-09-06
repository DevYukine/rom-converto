import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useCsoCompressStore = makeOpStore("cso-compress", () => ({
  input: "",
  output: "",
  format: "cso" as "cso" | "zso",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  blockSize: null as number | null,
  verifyAfter: false,
  recursive: true,
  maxDepth: null as number | null,
}));
