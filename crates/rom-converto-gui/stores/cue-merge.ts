import { defineStore } from "pinia";

export const useCueMergeStore = defineStore("cue-merge", () => {
  const input = ref("");
  const output = ref("");
  const force = ref(false);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    output.value = "";
    force.value = false;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    input,
    output,
    force,
    result,
    error,
    loading,
    $reset,
  };
});
