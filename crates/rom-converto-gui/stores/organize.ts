import { defineStore } from "pinia";
import { shallowRef } from "vue";
import { listen } from "~/lib/ipc";
import type { OrganizeRow, RunRow } from "~/types";
import { useUiStore } from "~/stores/ui";
import { MAX_LIVE_ROWS } from "./datScan";

const FLUSH_MS = 100;

export const useOrganizeStore = defineStore("organize", () => {
  const ui = useUiStore();
  const outputDir = ref("");
  const outputTemplate = ref("{console}/{basename}.{ext}");
  const dat = ref(false);
  const moveSource = ref(false);
  const playlists = ref(false);
  const allowEncrypted = ref(false);
  const maxDepth = ref<number | null>(null);
  const keys = ref("");
  const onConflict = ref(ui.defaultOnConflict);
  const skipSpaceCheck = ref(false);
  const statusFilter = ref<"all" | OrganizeRow["status"]>("all");
  // Rows the running organize streams on the op's fixed progress key. Events
  // arrive per library item, so they are buffered and folded into one array
  // on a timer: the result list re-renders a few times a second instead of
  // once per event. The index keyed by input is what replaces a unit's
  // pending row with its settled one.
  const liveRows = shallowRef<OrganizeRow[]>([]);
  const liveIndex = new Map<string, number>();
  let buffered: OrganizeRow[] = [];
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
      const i = liveIndex.get(row.input);
      if (i === undefined) {
        if (rows.length >= MAX_LIVE_ROWS) continue;
        liveIndex.set(row.input, rows.length);
        rows.push(row);
      } else {
        rows[i] = row;
      }
    }
    buffered = [];
    liveRows.value = rows;
  }

  function clearRunState() {
    if (flushTimer) clearTimeout(flushTimer);
    flushTimer = null;
    buffered = [];
    liveIndex.clear();
    liveRows.value = [];
    statusFilter.value = "all";
  }

  function ensureRowListener() {
    rowListener ??= listen<RunRow>("organize-row", (event) => {
      if (event.payload.kind !== "organize") return;
      buffered.push(event.payload);
      flushTimer ??= setTimeout(flushLiveRows, FLUSH_MS);
    }).then(() => undefined);
    return rowListener;
  }

  function $reset() {
    outputDir.value = "";
    outputTemplate.value = "{console}/{basename}.{ext}";
    dat.value = false;
    moveSource.value = false;
    playlists.value = false;
    allowEncrypted.value = false;
    maxDepth.value = null;
    keys.value = "";
    onConflict.value = ui.defaultOnConflict;
    skipSpaceCheck.value = false;
    clearRunState();
  }

  return {
    outputDir,
    outputTemplate,
    dat,
    moveSource,
    playlists,
    allowEncrypted,
    maxDepth,
    keys,
    onConflict,
    skipSpaceCheck,
    statusFilter,
    liveRows,
    clearRunState,
    flushLiveRows,
    ensureRowListener,
    $reset,
  };
});
