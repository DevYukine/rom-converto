import { defineStore } from "pinia";
import { useUiStore } from "~/stores/ui";

export const useNxSplitStore = defineStore("nx-split", () => {
  const ui = useUiStore();
  const keys = ref("");
  const outputDir = ref("");
  const onConflict = ref(ui.defaultOnConflict);
  const skipSpaceCheck = ref(false);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    keys.value = "";
    outputDir.value = "";
    onConflict.value = ui.defaultOnConflict;
    skipSpaceCheck.value = false;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    keys,
    outputDir,
    onConflict,
    skipSpaceCheck,
    result,
    error,
    loading,
    $reset,
  };
});
