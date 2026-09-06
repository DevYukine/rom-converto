import { makeOpStore } from "./_makeOpStore";

export const useCsoVerifyStore = makeOpStore("cso-verify", () => ({
  input: "",
  full: false,
}));
