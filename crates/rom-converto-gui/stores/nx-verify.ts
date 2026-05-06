import { defineStore } from "pinia";

export interface NcaVerdict {
  name: string;
  partition: string | null;
  ok: boolean;
  mismatched_sections: number;
}

export interface NxVerifyResult {
  kind: string;
  ok: boolean;
  ncas: NcaVerdict[];
}

export const useNxVerifyStore = defineStore("nx-verify", () => {
  const input = ref("");
  const keys = ref("");

  const verdict = ref<NxVerifyResult | null>(null);
  const error = ref("");
  const loading = ref(false);

  function $reset() {
    input.value = "";
    keys.value = "";
    verdict.value = null;
    error.value = "";
    loading.value = false;
  }

  return {
    input,
    keys,
    verdict,
    error,
    loading,
    $reset,
  };
});
