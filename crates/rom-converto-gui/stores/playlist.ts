import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const usePlaylistStore = makeOpStore("playlist", () => ({
  scanDir: "",
  outputDir: "",
  mode: "multiple",
  extensions: "cue,chd,iso,cso,zso",
  maxDepth: null as number | null,
  onConflict: useUiStore().defaultOnConflict,
}));
