export interface BatchItem {
  id: string;
  input: string;
  output: string;
  status: "pending" | "running" | "done" | "error" | "cancelled";
  error?: string;
  result?: string;
  /** Which worker slot is running this item (0-based). Unset when not running. */
  slot?: number;
  /** `Date.now()` when the item started running. */
  startedAt?: number;
  /** Wall-clock duration once the item reaches a terminal state. */
  elapsedMs?: number;
}
