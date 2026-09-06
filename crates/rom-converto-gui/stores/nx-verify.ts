import { makeOpStore } from "./_makeOpStore";

export interface NcaVerdict {
  name: string;
  partition: string | null;
  ok: boolean;
  mismatched_sections: number;
}

export interface NxVerifyResult {
  kind: string;
  ok: boolean;
  ncas: NcaVerdict[];
}

export const useNxVerifyStore = makeOpStore("nx-verify", () => ({
  input: "",
  keys: "",
  verdict: null as NxVerifyResult | null,
}));
