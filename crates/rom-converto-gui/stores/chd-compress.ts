import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

export const useChdCompressStore = defineStore("chd-compress", () => {
  const input = ref("");
  const output = ref("");
  const onConflict = ref("overwrite");
  const skipSpaceCheck = ref(false);
  const outputTemplate = ref("");
  const reportFile = ref("");
  const zstd = ref(false);
  const mode = ref<"auto" | "cd" | "dvd">("auto");
  const hunkSize = ref<number | null>(null);
  const verifyAfter = ref(false);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  const queue = ref<BatchItem[]>([]);
  const recursive = ref(true);
  const maxDepth = ref<number | null>(null);

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
    onConflict.value = "overwrite";
    skipSpaceCheck.value = false;
    outputTemplate.value = "";
    reportFile.value = "";
    mode.value = "auto";
    hunkSize.value = null;
    verifyAfter.value = false;
    result.value = "";
    error.value = "";
    loading.value = false;
    queue.value = [];
    recursive.value = true;
    maxDepth.value = null;
  }

  return {
    input,
    output,
    onConflict,
    skipSpaceCheck,
    outputTemplate,
    reportFile,
    zstd,
    mode,
    hunkSize,
    verifyAfter,
    result,
    error,
    loading,
    queue,
    recursive,
    maxDepth,
    addToQueue,
    removeFromQueue,
    clearQueue,
    $reset,
  };
});
