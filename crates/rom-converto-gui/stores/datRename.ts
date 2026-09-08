import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useDatRenameStore = makeOpStore("dat-rename", () => ({
  maxDepth: null as number | null,
  onConflict: useUiStore().defaultOnConflict,
}));
