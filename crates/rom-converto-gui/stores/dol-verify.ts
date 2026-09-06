import { makeOpStore } from "./_makeOpStore";

export interface DolStructuralReport {
  fst_offset: number;
  fst_size: number;
  fst_within_bounds: boolean;
  notes: string[];
}

export interface DolVerifyResult {
  game_id: string;
  rvz_structure: { ok: boolean } | null;
  structural: DolStructuralReport | null;
  disc_sha1: string | null;
  ok: boolean;
}

export const useDolVerifyStore = makeOpStore("dol-verify", () => ({
  input: "",
  full: false,
  verdict: null as DolVerifyResult | null,
}));
