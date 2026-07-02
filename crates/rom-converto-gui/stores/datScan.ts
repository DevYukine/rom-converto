import { defineStore } from "pinia";

export const useDatScanStore = defineStore("dat-scan", () => {
  const input = ref("");
  const maxDepth = ref<number | null>(null);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    maxDepth.value = null;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    input,
    maxDepth,
    result,
    error,
    loading,
    $reset,
  };
});
