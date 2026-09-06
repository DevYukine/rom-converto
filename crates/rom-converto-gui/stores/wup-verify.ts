import { makeOpStore } from "./_makeOpStore";

export interface TitleVerdict {
  title_id: number;
  title_id_hex: string;
  ok: boolean;
  verified_content: number;
  mismatched_content: number;
  skipped_content: number;
}

export interface WupVerifyResult {
  kind: string;
  ok: boolean;
  titles: TitleVerdict[];
}

export const useWupVerifyStore = makeOpStore("wup-verify", () => ({
  input: "",
  keys: "",
  verdict: null as WupVerifyResult | null,
}));
