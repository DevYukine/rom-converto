import { makeOpStore } from "./_makeOpStore";

export const useXenonVerifyStore = makeOpStore("xenon-verify", () => ({
  input: "",
}));
