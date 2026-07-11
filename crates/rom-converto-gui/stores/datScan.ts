import { defineStore } from "pinia";
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

export const useDatScanStore = defineStore("dat-scan", () => {
  const input = ref("");
  const maxDepth = ref<number | null>(null);
  const scanLevel = ref<ScanLevel>("crc");
  const quick = ref(false);
  const result = ref("");
  const error = ref("");
  const loading = ref(false);
  const commandLine = ref("");
  const statusFilter = ref<DatScanStatus | "all">("all");
  const scanResult = ref<DatScanResult | null>(null);
  const liveRows = ref(new Map<string, DatScanRowEvent>());
  let rowListener: Promise<void> | null = null;

  function clearScanState() {
    scanResult.value = null;
    liveRows.value.clear();
    statusFilter.value = "all";
  }

  function setLiveRow(row: DatScanRowEvent) {
    liveRows.value.set(row.path, row);
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
    result.value = "";
    error.value = "";
    loading.value = false;
    commandLine.value = "";
    clearScanState();
  }

  return {
    input,
    maxDepth,
    scanLevel,
    quick,
    result,
    error,
    loading,
    commandLine,
    statusFilter,
    scanResult,
    liveRows,
    clearScanState,
    setLiveRow,
    ensureRowListener,
    $reset,
  };
});
