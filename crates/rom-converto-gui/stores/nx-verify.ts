import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

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

export const useNxVerifyStore = defineStore("nx-verify", () => {
  const input = ref("");
  const keys = ref("");

  const verdict = ref<NxVerifyResult | null>(null);
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
    keys.value = "";
    verdict.value = null;
    result.value = "";
    error.value = "";
    loading.value = false;
    queue.value = [];
  }

  return {
    input,
    keys,
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
