import { defineStore } from "pinia";
import { useUiStore } from "~/stores/ui";

export const usePlaylistStore = defineStore("playlist", () => {
  const ui = useUiStore();
  const scanDir = ref("");
  const outputDir = ref("");
  const mode = ref("multiple");
  const extensions = ref("cue,chd,iso,cso,zso");
  const maxDepth = ref<number | null>(null);
  const onConflict = ref(ui.defaultOnConflict);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    scanDir.value = "";
    outputDir.value = "";
    mode.value = "multiple";
    extensions.value = "cue,chd,iso,cso,zso";
    maxDepth.value = null;
    onConflict.value = ui.defaultOnConflict;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    scanDir,
    outputDir,
    mode,
    extensions,
    maxDepth,
    onConflict,
    result,
    error,
    loading,
    $reset,
  };
});
