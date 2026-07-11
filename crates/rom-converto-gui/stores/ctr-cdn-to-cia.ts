import { defineStore } from "pinia";
import { useUiStore } from "~/stores/ui";

export const useCtrCdnToCiaStore = defineStore("ctr-cdn-to-cia", () => {
  const ui = useUiStore();
  const cdnDir = ref("");
  const output = ref("");
  const decrypt = ref(true);
  const compress = ref(false);
  const cleanup = ref(false);
  const recursive = ref(false);
  const ensureTicket = ref(true);
  const onConflict = ref(ui.defaultOnConflict);
  const skipSpaceCheck = ref(false);

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
    onConflict.value = ui.defaultOnConflict;
    skipSpaceCheck.value = false;
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
    onConflict,
    skipSpaceCheck,
    result,
    error,
    loading,
    $reset,
  };
});
