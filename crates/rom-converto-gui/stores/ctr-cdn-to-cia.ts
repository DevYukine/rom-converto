import { defineStore } from "pinia";

export const useCtrCdnToCiaStore = defineStore("ctr-cdn-to-cia", () => {
  const cdnDir = ref("");
  const output = ref("");
  const decrypt = ref(true);
  const compress = ref(false);
  const cleanup = ref(false);
  const recursive = ref(false);
  const ensureTicket = ref(true);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    cdnDir.value = "";
    output.value = "";
    decrypt.value = true;
    compress.value = false;
    cleanup.value = false;
    recursive.value = false;
    ensureTicket.value = true;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    cdnDir,
    output,
    decrypt,
    compress,
    cleanup,
    recursive,
    ensureTicket,
    result,
    error,
    loading,
    $reset,
  };
});
