import type { Ref } from "vue";
import { ref } from "vue";
import { invoke } from "@tauri-apps/api/core";
import type { BatchItem } from "~/types/batch";

export function useBatchOperation(
  taskId: string,
  commandName: string,
  buildArgs: (item: BatchItem) => Record<string, unknown>,
) {
  const running = ref(false);
  const currentIndex = ref(-1);
  const aborted = ref(false);

  const { concurrency, maxConcurrency } = useJobConcurrency();
  const progress = useProgress(taskId);
  // One progress channel per possible worker slot. The backend tags each
  // concurrent job's progress events with `${taskId}#${slot}`, so a slot's
  // channel only ever reflects whichever item is currently running in it.
  const progressSlots = Array.from({ length: maxConcurrency }, (_, slot) =>
    useProgress(`${taskId}#${slot}`),
  );

  async function start(
    queue: Ref<BatchItem[]>,
    resultRef: Ref<string>,
    options?: {
      isSuccess?: (result: string) => boolean;
      failureMessage?: (result: string) => string;
      errorRef?: Ref<string>;
    },
    onItemResult?: (res: unknown, item: BatchItem) => void,
    onItemError?: (item: BatchItem, error: string) => void | Promise<void>,
  ) {
    running.value = true;
    aborted.value = false;
    currentIndex.value = 0;

    resultRef.value = "";
    if (options?.errorRef) options.errorRef.value = "";
    // Failed and cancelled items stay in their sections; requeueing is an
    // explicit action (per-item or bulk retry in the queue UI), so a plain
    // rerun only picks up pending items.

    const notify = useBatchNotify();
    await notify.clearTaskbar();
    for (const slot of progressSlots) slot.reset();

    let doneCount = 0;
    let errorCount = 0;
    let cancelledCount = 0;
    let savedBytes = 0;
    let settledCount = 0;

    const total = queue.value.length;
    for (const item of queue.value) {
      if (item.status === "done") doneCount++;
      else if (item.status === "error") errorCount++;
      else if (item.status === "cancelled") cancelledCount++;
    }
    // Items already settled before this run (done from a prior pass, or
    // failed items left in place) count toward the taskbar fraction and the
    // final summary; without seeding, both under-report.
    settledCount = doneCount + errorCount + cancelledCount;
    if (settledCount > 0 && total > 0) {
      await notify.setTaskbarProgress(settledCount / total);
    }

    // Workers rescan for the lowest-index pending item each time they claim
    // work. Reading `queue.value` live (rather than a snapshot or a
    // monotonic cursor) lets items added mid-run still get picked up and
    // survives a drag-reorder rebuilding the array while workers are active.
    function claimNext(): BatchItem | null {
      for (const item of queue.value) {
        if (item.status === "pending") return item;
      }
      return null;
    }

    async function runSlot(slot: number) {
      const slotProgress = progressSlots[slot] ?? progress;
      for (;;) {
        if (aborted.value) return;
        const item = claimNext();
        if (!item) return;

        currentIndex.value = queue.value.indexOf(item);
        item.status = "running";
        item.slot = slot;
        item.startedAt = Date.now();
        slotProgress.reset({ keepWarnings: true });

        try {
          const args = { ...buildArgs(item), taskId: `${taskId}#${slot}` };
          const res = await invoke<unknown>(commandName, args);
          // Report-capable commands return a RunOutcome object (message, record,
          // input_bytes, output_bytes); the rest return a plain string. Display
          // the message either way.
          const display =
            typeof res === "object" && res !== null
              ? String((res as { message?: unknown }).message ?? res)
              : String(res);
          if (options?.isSuccess && !options.isSuccess(display)) {
            item.status = "error";
            item.result = display;
            item.error = options.failureMessage
              ? options.failureMessage(display)
              : "verification failed";
            errorCount++;
          } else {
            item.status = "done";
            item.result = display;
            onItemResult?.(res, item);
            doneCount++;

            // input_bytes/output_bytes are always present on report-capable
            // commands (unlike `record`, which only exists when reporting is on),
            // so the saved-space summary works without the user enabling reports.
            const outcome =
              typeof res === "object" && res !== null
                ? (res as { input_bytes?: unknown; output_bytes?: unknown })
                : null;
            if (outcome && typeof outcome.input_bytes === "number" && typeof outcome.output_bytes === "number") {
              savedBytes += Math.max(0, outcome.input_bytes - outcome.output_bytes);
            }
          }
        } catch (e) {
          const message = String(e);
          if (message.includes("operation cancelled")) {
            item.status = "cancelled";
            item.error = message;
            cancelledCount++;
          } else {
            item.status = "error";
            item.error = message;
            errorCount++;
            await onItemError?.(item, message);
          }
        }

        item.elapsedMs = Date.now() - (item.startedAt ?? Date.now());
        settledCount++;
        await notify.setTaskbarProgress(settledCount / total);
      }
    }

    const workerCount = Math.max(1, Math.min(concurrency.value, maxConcurrency, total || 1));
    await Promise.all(Array.from({ length: workerCount }, (_, slot) => runSlot(slot)));

    running.value = false;
    currentIndex.value = -1;

    if (errorCount === 0 && cancelledCount === 0) {
      resultRef.value = `All ${total} files processed successfully`;
    } else {
      let summary = `${doneCount} of ${total} succeeded`;
      if (errorCount > 0) summary += `, ${errorCount} failed`;
      if (cancelledCount > 0) summary += `, ${cancelledCount} cancelled`;
      resultRef.value = summary;
    }

    if (total > 0) {
      const { soundEnabled } = useNotifyPrefs();
      let body = `${doneCount} of ${total} done`;
      if (errorCount > 0) body += `, ${errorCount} failed`;
      if (cancelledCount > 0) body += `, ${cancelledCount} cancelled`;
      if (savedBytes > 0) body += `, ${notify.formatBytes(savedBytes)} saved`;
      if (!document.hasFocus()) {
        const title = errorCount > 0 ? "Batch finished with errors" : "Batch finished";
        await notify.notifyBatchDone(title, body, soundEnabled.value);
      }
      if (errorCount > 0) {
        await notify.taskbarError();
      } else {
        await notify.clearTaskbar();
      }
    }
  }

  async function abort() {
    aborted.value = true;
    try {
      await invoke("cmd_cancel", { taskId: null });
    } catch {
      // The running tasks may have already finished; nothing to cancel.
    }
    await useBatchNotify().clearTaskbar();
  }

  function removeSelected(queue: Ref<BatchItem[]>, ids: string[]) {
    const idSet = new Set(ids);
    queue.value = queue.value.filter((item) => !idSet.has(item.id));
  }

  // `orderedPendingIds` is the new order for the pending subset only (the
  // only items a drag-reorder can touch); running/done/error items keep
  // their existing position in the queue.
  function reorder(queue: Ref<BatchItem[]>, orderedPendingIds: string[]) {
    const byId = new Map(queue.value.map((item) => [item.id, item]));
    let cursor = 0;
    queue.value = queue.value.map((item) => {
      if (item.status !== "pending") return item;
      const nextId = orderedPendingIds[cursor++];
      return (nextId && byId.get(nextId)) || item;
    });
  }

  return { running, currentIndex, progress, progressSlots, start, abort, removeSelected, reorder };
}
