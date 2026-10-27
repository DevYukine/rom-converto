import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const usePsxToPbpStore = makeOpStore("psx-to-pbp", () => ({
  input: "",
  outputDir: "",
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  compression: "deflate",
  title: "",
  discId: "",
  splash: "",
  icon0: "",
  pic1: "",
  reportFile: "",
}));
