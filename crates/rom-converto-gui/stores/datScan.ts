import { defineStore } from "pinia";
import { shallowRef } from "vue";
import { listen } from "~/lib/ipc";
import type { DatScanRow, RunRow } from "~/types";

export type ScanLevel = "crc" | "md5" | "sha1" | "sha256";

// Cap on rows held from the live stream. The settled result payload still
// carries every row, so a scan of a huge library stays complete without the
// page growing without bound while it runs.
export const MAX_LIVE_ROWS = 5000;

const FLUSH_MS = 100;

export const useDatScanStore = defineStore("dat-scan", () => {
  const maxDepth = ref<number | null>(null);
  const scanLevel = ref<ScanLevel>("crc");
  const quick = ref(false);
  const statusFilter = ref<string>("all");
  // Rows the running scan streams on the op's fixed progress key. Events
  // arrive per file, thousands of times for folders of small files, so they
  // are buffered and folded into one array on a timer: the result list
  // re-renders a few times a second instead of once per event. The index
  // keyed by path is what replaces a unit's "pending" row with its settled
  // one.
  const liveRows = shallowRef<DatScanRow[]>([]);
  const liveIndex = new Map<string, number>();
  let buffered: DatScanRow[] = [];
  let flushTimer: ReturnType<typeof setTimeout> | null = null;
  let rowListener: Promise<void> | null = null;

  // Replaces the array rather than mutating it: a computed that returns the
  // same array identity does not notify its own dependents.
  function flushLiveRows() {
    if (flushTimer) clearTimeout(flushTimer);
    flushTimer = null;
    if (!buffered.length) return;
    const rows = liveRows.value.slice();
    for (const row of buffered) {
      const i = liveIndex.get(row.path);
      if (i === undefined) {
        if (rows.length >= MAX_LIVE_ROWS) continue;
        liveIndex.set(row.path, rows.length);
        rows.push(row);
      } else {
        rows[i] = row;
      }
    }
    buffered = [];
    liveRows.value = rows;
  }

  function clearScanState() {
    if (flushTimer) clearTimeout(flushTimer);
    flushTimer = null;
    buffered = [];
    liveIndex.clear();
    liveRows.value = [];
    statusFilter.value = "all";
  }

  function ensureRowListener() {
    rowListener ??= listen<RunRow>("dat-scan-row", (event) => {
      if (event.payload.kind !== "dat_scan") return;
      buffered.push(event.payload);
      flushTimer ??= setTimeout(flushLiveRows, FLUSH_MS);
    }).then(() => undefined);
    return rowListener;
  }

  function $reset() {
    maxDepth.value = null;
    scanLevel.value = "crc";
    quick.value = false;
    clearScanState();
  }

  return {
    maxDepth,
    scanLevel,
    quick,
    statusFilter,
    liveRows,
    clearScanState,
    flushLiveRows,
    ensureRowListener,
    $reset,
  };
});
