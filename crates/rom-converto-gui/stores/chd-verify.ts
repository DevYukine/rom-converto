import { makeOpStore } from "./_makeOpStore";

export const useChdVerifyStore = makeOpStore("chd-verify", () => ({
  input: "",
  parent: "",
  fix: false,
}));
