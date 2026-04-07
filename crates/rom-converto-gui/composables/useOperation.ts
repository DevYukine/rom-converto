import type { Ref } from "vue";
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";

interface OperationRefs {
  result: Ref<string>;
  error: Ref<string>;
  loading: Ref<boolean>;
}

export function useOperation(refs?: OperationRefs) {
  const result = refs?.result ?? ref("");
  const error = refs?.error ?? ref("");
  const loading = refs?.loading ?? ref(false);

  async function run<T extends Record<string, unknown>>(
    command: string,
    args: T,
  ) {
    result.value = "";
    error.value = "";
    loading.value = true;

    try {
      const res = await invoke<string>(command, args);
      result.value = res;
    } catch (e) {
      error.value = String(e);
    } finally {
      loading.value = false;
    }
  }

  function reset() {
    result.value = "";
    error.value = "";
    loading.value = false;
  }

  return { result, error, loading, run, reset };
}
