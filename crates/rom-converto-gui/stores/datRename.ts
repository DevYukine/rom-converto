import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useDatRenameStore = makeOpStore("dat-rename", () => ({
  input: "",
  maxDepth: null as number | null,
  onConflict: useUiStore().defaultOnConflict,
  error: "",
  loading: false,
}));
