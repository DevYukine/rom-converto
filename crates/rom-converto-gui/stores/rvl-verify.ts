import { makeOpStore } from "./_makeOpStore";

export interface RvlPartitionVerify {
  offset: number;
  partition_type: number;
  kind: string;
  clusters_checked: number;
  mismatched_clusters: number;
  scrubbed_clusters: number;
  sample_bad_clusters: number[];
  ok: boolean;
  note: string | null;
}

export interface RvlVerifyResult {
  game_id: string;
  rvz_structure: { ok: boolean } | null;
  partitions: RvlPartitionVerify[];
  ok: boolean;
}

export const useRvlVerifyStore = makeOpStore("rvl-verify", () => ({
  input: "",
  full: false,
  verdict: null as RvlVerifyResult | null,
}));
