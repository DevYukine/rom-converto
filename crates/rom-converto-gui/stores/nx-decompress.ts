import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

export const useNxDecompressStore = defineStore("nx-decompress", () => {
  const queue = ref<BatchItem[]>([]);
  const output = ref("");
  const keys = ref("");

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function addToQueue(inputPath: string) {
    if (queue.value.some((i) => i.input === inputPath)) return;
    queue.value.push({
      id: crypto.randomUUID(),
      input: inputPath,
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
    queue.value = [];
    output.value = "";
    keys.value = "";
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    queue,
    output,
    keys,
    result,
    error,
    loading,
    addToQueue,
    removeFromQueue,
    clearQueue,
    $reset,
  };
});
