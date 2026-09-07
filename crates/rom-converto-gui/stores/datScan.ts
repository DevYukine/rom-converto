import { defineStore } from "pinia";
import { shallowRef } from "vue";
import { listen } from "~/lib/ipc";

export type ScanLevel = "crc" | "md5" | "sha1" | "sha256";
export type DatScanStatus = "matched" | "misnamed" | "hint" | "unknown" | "unsupported" | "failed";

export interface DatScanRow {
  path: string;
  status: DatScanStatus;
  gameName: string | null;
  canonicalStem: string | null;
  error: string | null;
}

// Live per-file event streamed while the scan is still running; "pending" is
// emitted as soon as a file is digested, before the bulk match query settles.
export interface DatScanRowEvent extends Omit<DatScanRow, "status"> {
  status: DatScanStatus | "pending";
}

export interface DatScanResult {
  kind: "scan";
  matched: number;
  misnamed: number;
  hint: number;
  unknown: number;
  unsupported: number;
  failed: number;
  rows: DatScanRow[];
}

const FLUSH_MS = 100;

export const useDatScanStore = defineStore("dat-scan", () => {
  const input = ref("");
  const maxDepth = ref<number | null>(null);
  const scanLevel = ref<ScanLevel>("crc");
  const quick = ref(false);
  const commandLine = ref("");
  const statusFilter = ref<DatScanStatus | "pending" | "all">("all");
  const scanResult = ref<DatScanResult | null>(null);
  // Row events arrive per file, thousands of times for folders of small
  // files. They are buffered and folded into one array on a timer so the
  // result list re-renders a few times a second instead of per event.
  const liveRows = shallowRef<DatScanRowEvent[]>([]);
  const liveIndex = new Map<string, number>();
  let pending: DatScanRowEvent[] = [];
  let flushTimer: ReturnType<typeof setTimeout> | null = null;
  const error = ref("");
  const loading = ref(false);
  const startedAt = ref(0);
  const finishedAt = ref(0);
  let rowListener: Promise<void> | null = null;

  // Replaces the array rather than mutating it: a computed that returns the
  // same array identity does not notify its own dependents.
  function flushLiveRows() {
    if (flushTimer) clearTimeout(flushTimer);
    flushTimer = null;
    if (!pending.length) return;
    const rows = liveRows.value.slice();
    for (const row of pending) {
      const i = liveIndex.get(row.path);
      if (i === undefined) {
        liveIndex.set(row.path, rows.length);
        rows.push(row);
      } else {
        rows[i] = row;
      }
    }
    pending = [];
    liveRows.value = rows;
  }

  function clearScanState() {
    scanResult.value = null;
    if (flushTimer) clearTimeout(flushTimer);
    flushTimer = null;
    pending = [];
    liveIndex.clear();
    liveRows.value = [];
    statusFilter.value = "all";
  }

  function setLiveRow(row: DatScanRowEvent) {
    pending.push(row);
    flushTimer ??= setTimeout(flushLiveRows, FLUSH_MS);
  }

  function ensureRowListener() {
    rowListener ??= listen<DatScanRowEvent>("dat-scan-row", (event) => {
      setLiveRow(event.payload);
    }).then(() => undefined);
    return rowListener;
  }

  function $reset() {
    input.value = "";
    maxDepth.value = null;
    scanLevel.value = "crc";
    quick.value = false;
    commandLine.value = "";
    error.value = "";
    loading.value = false;
    startedAt.value = 0;
    finishedAt.value = 0;
    clearScanState();
  }

  return {
    input,
    maxDepth,
    scanLevel,
    quick,
    commandLine,
    statusFilter,
    scanResult,
    liveRows,
    error,
    loading,
    startedAt,
    finishedAt,
    clearScanState,
    setLiveRow,
    flushLiveRows,
    ensureRowListener,
    $reset,
  };
});
