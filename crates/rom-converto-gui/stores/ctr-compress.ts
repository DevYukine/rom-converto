import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

export const useCtrCompressStore = defineStore("ctr-compress", () => {
  const input = ref("");
  const output = ref("");
  // Zstd compression level: 0 = library default, 1..22 = explicit.
  // Sent straight to the backend; the lib treats 0 as "use default".
  const level = ref<number>(0);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  const queue = ref<BatchItem[]>([]);

  function addToQueue(filePath: string, outputPath: string) {
    queue.value.push({
      id: crypto.randomUUID(),
      input: filePath,
      output: outputPath,
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
    output.value = "";
    level.value = 0;
    result.value = "";
    error.value = "";
    loading.value = false;
    queue.value = [];
  }

  return {
    input,
    output,
    level,
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
