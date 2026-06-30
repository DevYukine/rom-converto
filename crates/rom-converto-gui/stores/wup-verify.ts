import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

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

export const useWupVerifyStore = defineStore("wup-verify", () => {
  const input = ref("");
  const keys = ref("");

  const verdict = ref<WupVerifyResult | null>(null);
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
