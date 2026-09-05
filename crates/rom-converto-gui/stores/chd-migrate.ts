import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";
import { useUiStore } from "~/stores/ui";

export const useChdMigrateStore = defineStore("chd-migrate", () => {
  const ui = useUiStore();
  const input = ref("");
  const output = ref("");
  const onConflict = ref(ui.defaultOnConflict);
  const skipSpaceCheck = ref(false);
  const outputTemplate = ref("");
  const reportFile = ref("");
  const codecs = ref<string[]>([]);
  const level = ref<number | null>(null);
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
    codecs.value = [];
    level.value = null;
    onConflict.value = ui.defaultOnConflict;
    skipSpaceCheck.value = false;
    outputTemplate.value = "";
    reportFile.value = "";
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
    codecs,
    level,
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
