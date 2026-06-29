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
  const cancelled = ref(false);

  async function run<T extends Record<string, unknown>>(
    command: string,
    args: T,
  ) {
    result.value = "";
    error.value = "";
    cancelled.value = false;
    loading.value = true;

    try {
      const res = await invoke<string>(command, args);
      result.value = res;
    } catch (e) {
      const message = String(e);
      if (message.includes("operation cancelled")) {
        cancelled.value = true;
      } else {
        error.value = message;
      }
    } finally {
      loading.value = false;
    }
  }

  async function abort() {
    try {
      await invoke("cmd_cancel");
    } catch {
      // The running task may have already finished; nothing to cancel.
    }
  }

  function reset() {
    result.value = "";
    error.value = "";
    cancelled.value = false;
    loading.value = false;
  }

  return { result, error, loading, cancelled, run, reset, abort };
}
