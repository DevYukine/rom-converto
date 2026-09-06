import { makeOpStore } from "./_makeOpStore";

export const useHashStore = makeOpStore("hash", () => ({
  input: "",
  algos: ["crc32", "sha1"] as string[],
  recursive: false,
  maxDepth: null as number | null,
}));
