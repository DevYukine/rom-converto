import { defineStore } from "pinia";

export const useCueMergeStore = defineStore("cue-merge", () => {
  const input = ref("");
  const output = ref("");
  const onConflict = ref("overwrite");
  const skipSpaceCheck = ref(false);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    output.value = "";
    onConflict.value = "overwrite";
    skipSpaceCheck.value = false;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    input,
    output,
    onConflict,
    skipSpaceCheck,
    result,
    error,
    loading,
    $reset,
  };
});
