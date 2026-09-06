import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useChdCompressStore = makeOpStore("chd-compress", () => ({
  input: "",
  output: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  codecs: [] as string[],
  level: null as number | null,
  mode: "auto" as "auto" | "cd" | "dvd" | "ld",
  hunkSize: null as number | null,
  verifyAfter: false,
  recursive: true,
  maxDepth: null as number | null,
}));
