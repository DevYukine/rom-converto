import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

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

export const useRvlVerifyStore = defineStore("rvl-verify", () => {
  const input = ref("");
  const full = ref(false);

  const verdict = ref<RvlVerifyResult | null>(null);
  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  const queue = ref<BatchItem[]>([]);

  function addToQueue(filePath: string) {
    queue.value.push({
      id: crypto.randomUUID(),
      input: filePath,
      output: "",
      status: "pending",
    });
  }

  function removeFromQueue(id: string) {
    queue.value = queue.value.filter((item) => item.id !== id);
  }

  function clearQueue() {
    queue.value = [];
  }

  function $reset() {
    input.value = "";
    full.value = false;
    verdict.value = null;
    result.value = "";
    error.value = "";
    loading.value = false;
    queue.value = [];
  }

  return {
    input,
    full,
    verdict,
    result,
    error,
    loading,
    queue,
    addToQueue,
    removeFromQueue,
    clearQueue,
    $reset,
  };
});
