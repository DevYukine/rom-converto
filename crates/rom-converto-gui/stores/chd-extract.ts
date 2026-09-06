import { makeOpStore } from "./_makeOpStore";

export const useChdExtractStore = makeOpStore("chd-extract", () => ({
  input: "",
  output: "",
  parent: "",
  skipSpaceCheck: false,
  outputTemplate: "",
  reportFile: "",
  recursive: true,
  maxDepth: null as number | null,
}));
