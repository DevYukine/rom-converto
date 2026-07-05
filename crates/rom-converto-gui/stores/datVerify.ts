import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

export const useDatVerifyStore = defineStore("dat-verify", () => {
  const input = ref("");
  const quick = ref(false);

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
    quick.value = false;
    result.value = "";
    error.value = "";
    loading.value = false;
    queue.value = [];
  }

  return {
    input,
    quick,
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
