// Mirrors rom_converto_lib::util::ReportRecord and ReportTotals so records
// collected per file can be handed back to cmd_write_report unchanged. The
// report bytes themselves are produced by the library's write_report, never
// here.
export interface ReportRecord {
  input_path: string;
  output_path: string;
  operation: string;
  status: "ok" | "skipped" | "failed";
  input_bytes: number;
  output_bytes: number;
  ratio_pct: number | null;
  elapsed_ms: number;
  error: string | null;
}

export interface ReportTotals {
  total_files: number;
  ok: number;
  skipped: number;
  failed: number;
  total_input_bytes: number;
  total_output_bytes: number;
  elapsed_ms: number;
}

export interface RunOutcome {
  message: string;
  record: ReportRecord | null;
  input_bytes: number;
  output_bytes: number;
}
