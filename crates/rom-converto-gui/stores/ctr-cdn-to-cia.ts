import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useCtrCdnToCiaStore = makeOpStore("ctr-cdn-to-cia", () => ({
  cdnDir: "",
  output: "",
  decrypt: true,
  compress: false,
  cleanup: false,
  recursive: false,
  ensureTicket: true,
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
}));
