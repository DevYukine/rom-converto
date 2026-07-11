import { defineStore } from "pinia";
import { useUiStore } from "~/stores/ui";

export const useDatRenameStore = defineStore("dat-rename", () => {
  const ui = useUiStore();
  const input = ref("");
  const maxDepth = ref<number | null>(null);
  const dryRun = ref(true);
  const onConflict = ref(ui.defaultOnConflict);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    maxDepth.value = null;
    dryRun.value = true;
    onConflict.value = ui.defaultOnConflict;
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
