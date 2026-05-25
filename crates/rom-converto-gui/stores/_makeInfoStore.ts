// Shared shape across the six per-console info stores so the sidebar
// status dot can read loading/result/error the same way it does for
// the other operations.

import { defineStore } from "pinia";
import { invoke } from "@tauri-apps/api/core";
import type { InfoResult } from "~/types/info";

export function makeInfoStore(id: string) {
  return defineStore(id, () => {
    const input = ref("");
    const keys = ref("");

    const info = ref<InfoResult | null>(null);
    const rawJson = ref("");
    const result = ref("");
    const error = ref("");
    const loading = ref(false);

    async function execute() {
      if (!input.value) {
        error.value = "Pick a file or directory first";
        return;
      }
      loading.value = true;
      error.value = "";
      info.value = null;
      rawJson.value = "";
      result.value = "";
      try {
        const json = await invoke<string>("cmd_read_info", {
          input: input.value,
          keys: keys.value || null,
        });
        rawJson.value = json;
        info.value = JSON.parse(json) as InfoResult;
        result.value = "Info loaded";
      } catch (e) {
        error.value = String(e);
      } finally {
        loading.value = false;
      }
    }

    function $reset() {
      input.value = "";
      keys.value = "";
      info.value = null;
      rawJson.value = "";
      result.value = "";
      error.value = "";
      loading.value = false;
    }

    return {
      input,
      keys,
      info,
      rawJson,
      result,
      error,
      loading,
      execute,
      $reset,
    };
  });
}
