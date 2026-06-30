import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

export const useDolCompressStore = defineStore("dol-compress", () => {
  const input = ref("");
  const output = ref("");
  const level = ref(22);
  const chunkSize = ref(131072);
  const onConflict = ref("overwrite");
  const skipSpaceCheck = ref(false);

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
    level.value = 22;
    chunkSize.value = 131072;
    onConflict.value = "overwrite";
    skipSpaceCheck.value = false;
    result.value = "";
    error.value = "";
    loading.value = false;
    queue.value = [];
  }

  return {
    input,
    output,
    level,
    chunkSize,
    onConflict,
    skipSpaceCheck,
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
