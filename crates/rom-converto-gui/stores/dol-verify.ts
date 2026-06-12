import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

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

export const useDolVerifyStore = defineStore("dol-verify", () => {
  const input = ref("");
  const full = ref(false);

  const verdict = ref<DolVerifyResult | null>(null);
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
