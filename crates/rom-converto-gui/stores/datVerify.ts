import { defineStore } from "pinia";
import { listen } from "~/lib/ipc";
import { MAX_LIVE_ROWS } from "./datScan";
import type { DatMatchData, RunRow } from "~/types";

export const useDatVerifyStore = defineStore("dat-verify", () => {
  const quick = ref(false);
  // Rows a directory verify streams on the op's fixed progress key, keyed by
  // path. A single-file verify streams nothing and settles as one payload.
  const liveRows = ref(new Map<string, DatMatchData>());
  let rowListener: Promise<void> | null = null;

  function ensureRowListener() {
    rowListener ??= listen<RunRow>("dat-verify-row", (event) => {
      if (event.payload.kind !== "dat_match") return;
      const row = event.payload;
      if (!liveRows.value.has(row.path) && liveRows.value.size >= MAX_LIVE_ROWS) return;
      liveRows.value.set(row.path, row);
    }).then(() => undefined);
    return rowListener;
  }

  function $reset() {
    quick.value = false;
    liveRows.value.clear();
  }

  return { quick, liveRows, ensureRowListener, $reset };
});
