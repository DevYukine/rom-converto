import { makeOpStore } from "./_makeOpStore";
import { useUiStore } from "~/stores/ui";

export const useCtrCompressStore = makeOpStore("ctr-compress", () => ({
  input: "",
  output: "",
  // Zstd compression level: 0 = library default, 1..22 = explicit.
  // Sent straight to the backend; the lib treats 0 as "use default".
  level: 0,
  allowEncrypted: false,
  onConflict: useUiStore().defaultOnConflict,
  skipSpaceCheck: false,
  outputTemplate: "",
  recursive: true,
  maxDepth: null as number | null,
}));
