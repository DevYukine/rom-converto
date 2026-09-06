import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useChdMigrateStore = makeOpStore("chd-migrate", () => ({
  input: "",
  output: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  codecs: [] as string[],
  level: null as number | null,
  hunkSize: null as number | null,
  verifyAfter: false,
  recursive: true,
  maxDepth: null as number | null,
}));
