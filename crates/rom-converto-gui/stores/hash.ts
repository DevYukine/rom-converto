import { defineStore } from "pinia";

export const useHashStore = defineStore("hash", () => {
  const input = ref("");
  const algos = ref<string[]>(["crc32", "sha1"]);
  const recursive = ref(false);
  const maxDepth = ref<number | null>(null);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    algos.value = ["crc32", "sha1"];
    recursive.value = false;
    maxDepth.value = null;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    input,
    algos,
    recursive,
    maxDepth,
    result,
    error,
    loading,
    $reset,
  };
});
