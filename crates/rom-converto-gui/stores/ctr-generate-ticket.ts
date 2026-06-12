import { defineStore } from "pinia";

export const useCtrGenerateTicketStore = defineStore("ctr-generate-ticket", () => {
  const cdnDir = ref("");
  const output = ref("");

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    cdnDir.value = "";
    output.value = "";
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    cdnDir,
    output,
    result,
    error,
    loading,
    $reset,
  };
});
