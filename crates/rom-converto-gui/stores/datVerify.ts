import { makeOpStore } from "./_makeOpStore";

export const useDatVerifyStore = makeOpStore("dat-verify", () => ({
  quick: false,
}));
