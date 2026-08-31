import { defineStore } from "pinia";
import { useUiStore } from "~/stores/ui";

export const useCueConvertStore = defineStore("cue-convert", () => {
  const ui = useUiStore();
  const input = ref("");
  const format = ref<"iso" | "cso" | "zso">("zso");
  const onConflict = ref(ui.defaultOnConflict);
  const skipSpaceCheck = ref(false);
  const recursive = ref(true);
  const maxDepth = ref<number | null>(null);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    format.value = "zso";
    onConflict.value = ui.defaultOnConflict;
    skipSpaceCheck.value = false;
    recursive.value = true;
    maxDepth.value = null;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    input,
    format,
    recursive,
    maxDepth,
    onConflict,
    skipSpaceCheck,
    result,
    error,
    loading,
    $reset,
  };
});
