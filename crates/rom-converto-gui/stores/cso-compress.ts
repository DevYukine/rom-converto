import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

export const useCsoCompressStore = defineStore("cso-compress", () => {
  const input = ref("");
  const output = ref("");
  const format = ref<"cso" | "zso">("cso");
  const force = ref(false);
  const blockSize = ref<number | null>(null);

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
    format.value = "cso";
    force.value = false;
    blockSize.value = null;
    result.value = "";
    error.value = "";
    loading.value = false;
    queue.value = [];
  }

  return {
    input,
    output,
    format,
    force,
    blockSize,
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
