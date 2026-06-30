import { defineStore } from "pinia";
import type { BatchItem } from "~/types/batch";

export const useCtrCompressStore = defineStore("ctr-compress", () => {
  const input = ref("");
  const output = ref("");
  // Zstd compression level: 0 = library default, 1..22 = explicit.
  // Sent straight to the backend; the lib treats 0 as "use default".
  const level = ref<number>(0);
  const allowEncrypted = ref<boolean>(false);
  const onConflict = ref("overwrite");
  const skipSpaceCheck = ref(false);
  const outputTemplate = ref("");

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
    allowEncrypted.value = false;
    onConflict.value = "overwrite";
    skipSpaceCheck.value = false;
    outputTemplate.value = "";
    result.value = "";
    error.value = "";
    loading.value = false;
    queue.value = [];
  }

  return {
    input,
    output,
    level,
    allowEncrypted,
    onConflict,
    skipSpaceCheck,
    outputTemplate,
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
