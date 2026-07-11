import { defineStore } from "pinia";
import { useUiStore } from "~/stores/ui";

export const useWupDecryptStore = defineStore("wup-decrypt", () => {
  const ui = useUiStore();
  const input = ref("");
  const output = ref("");
  const onConflict = ref(ui.defaultOnConflict);
  const skipSpaceCheck = ref(false);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    output.value = "";
    onConflict.value = ui.defaultOnConflict;
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
