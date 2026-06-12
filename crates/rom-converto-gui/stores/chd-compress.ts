import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

export const useChdCompressStore = defineStore("chd-compress", () => {
  const input = ref("");
  const output = ref("");
  const force = ref(false);
  const zstd = ref(false);
  const mode = ref<"auto" | "cd" | "dvd">("auto");
  const hunkSize = ref<number | null>(null);

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
    zstd.value = false;
    force.value = false;
    mode.value = "auto";
    hunkSize.value = null;
    result.value = "";
    error.value = "";
    loading.value = false;
    queue.value = [];
  }

  return {
    input,
    output,
    force,
    zstd,
    mode,
    hunkSize,
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
