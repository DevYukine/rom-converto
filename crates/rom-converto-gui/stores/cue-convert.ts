import { defineStore } from "pinia";

export const useCueConvertStore = defineStore("cue-convert", () => {
  const input = ref("");
  const output = ref("");
  const format = ref<"iso" | "cso" | "zso">("zso");
  const onConflict = ref("overwrite");
  const skipSpaceCheck = ref(false);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    output.value = "";
    format.value = "zso";
    onConflict.value = "overwrite";
    skipSpaceCheck.value = false;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    input,
    output,
    format,
    onConflict,
    skipSpaceCheck,
    result,
    error,
    loading,
    $reset,
  };
});
