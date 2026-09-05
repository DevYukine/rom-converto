import { defineStore } from "pinia";
import { useUiStore } from "~/stores/ui";

export const useNxMergeStore = defineStore("nx-merge", () => {
  const ui = useUiStore();
  const output = ref("");
  const keys = ref("");
  const format = ref("nsp");
  const onConflict = ref(ui.defaultOnConflict);
  const skipSpaceCheck = ref(false);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    output.value = "";
    keys.value = "";
    format.value = "nsp";
    onConflict.value = ui.defaultOnConflict;
    skipSpaceCheck.value = false;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    output,
    keys,
    format,
    onConflict,
    skipSpaceCheck,
    result,
    error,
    loading,
    $reset,
  };
});
