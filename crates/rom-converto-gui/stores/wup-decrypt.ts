import { defineStore } from "pinia";

export const useWupDecryptStore = defineStore("wup-decrypt", () => {
  const input = ref("");
  const output = ref("");

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    output.value = "";
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    input,
    output,
    result,
    error,
    loading,
    $reset,
  };
});
