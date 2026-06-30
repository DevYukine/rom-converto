import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

export const useNxDecompressStore = defineStore("nx-decompress", () => {
  const queue = ref<BatchItem[]>([]);
  const recursive = ref(true);
  const maxDepth = ref<number | null>(null);
  const output = ref("");
  const keys = ref("");
  const onConflict = ref("overwrite");
  const skipSpaceCheck = ref(false);
  const outputTemplate = ref("");
  const reportFile = ref("");

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
    recursive.value = true;
    maxDepth.value = null;
    output.value = "";
    keys.value = "";
    onConflict.value = "overwrite";
    skipSpaceCheck.value = false;
    outputTemplate.value = "";
    reportFile.value = "";
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    queue,
    recursive,
    maxDepth,
    output,
    keys,
    onConflict,
    skipSpaceCheck,
    outputTemplate,
    reportFile,
    result,
    error,
    loading,
    addToQueue,
    removeFromQueue,
    clearQueue,
    $reset,
  };
});
