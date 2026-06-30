import { defineStore } from "pinia";

export const usePlaylistStore = defineStore("playlist", () => {
  const scanDir = ref("");
  const outputDir = ref("");
  const mode = ref("multiple");
  const extensions = ref("cue,chd,iso,cso,zso");
  const maxDepth = ref<number | null>(null);
  const onConflict = ref("overwrite");

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    scanDir.value = "";
    outputDir.value = "";
    mode.value = "multiple";
    extensions.value = "cue,chd,iso,cso,zso";
    maxDepth.value = null;
    onConflict.value = "overwrite";
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
