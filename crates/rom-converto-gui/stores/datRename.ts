import { defineStore } from "pinia";

export const useDatRenameStore = defineStore("dat-rename", () => {
  const input = ref("");
  const maxDepth = ref<number | null>(null);
  const dryRun = ref(true);
  const onConflict = ref("overwrite");

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    maxDepth.value = null;
    dryRun.value = true;
    onConflict.value = "overwrite";
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    input,
    maxDepth,
    dryRun,
    onConflict,
    result,
    error,
    loading,
    $reset,
  };
});
