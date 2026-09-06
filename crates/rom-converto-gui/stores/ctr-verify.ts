import { makeOpStore } from "./_makeOpStore";

export const useCtrVerifyStore = makeOpStore("ctr-verify", () => ({
  input: "",
  verifyContent: false,
}));
