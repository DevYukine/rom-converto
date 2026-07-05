import { defineStore } from "pinia";

export type ScanLevel = "crc" | "md5" | "sha1" | "sha256";

export const useDatScanStore = defineStore("dat-scan", () => {
  const input = ref("");
  const maxDepth = ref<number | null>(null);
  const scanLevel = ref<ScanLevel>("crc");
  const quick = ref(false);

  const result = ref("");
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    maxDepth.value = null;
    scanLevel.value = "crc";
    quick.value = false;
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return {
    input,
    maxDepth,
    scanLevel,
    quick,
    result,
    error,
    loading,
    $reset,
  };
});
