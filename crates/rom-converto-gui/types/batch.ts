// The CLI exposes `--report` (CSV/JSON/HTML run reports), but the GUI has no
// equivalent yet. Wiring it in needs: per-file size and elapsed fields here
// (input_bytes, output_bytes, elapsed_ms), each Tauri command returning those
// figures instead of a plain success string, useBatchOperation storing them on
// the item, and a cmd_save_report Tauri command that forwards the collected
// records plus a destination path to rom_converto_lib::util::write_report.
export interface BatchItem {
  id: string;
  input: string;
  output: string;
  status: "pending" | "running" | "done" | "error" | "cancelled";
  error?: string;
  result?: string;
}
