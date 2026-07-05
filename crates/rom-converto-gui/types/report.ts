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

// Mirrors rom_converto_gui's VerifyReport: the post-conversion check
// verdict, whether that check re-decodes the whole output (round trip) or
// only inspects structure, and a short message for the comparison card.
export interface VerifyReport {
  ok: boolean;
  round_trip: boolean;
  message: string;
}

// Before/after summary for one conversion, populated on every successful run
// regardless of the report toggle. `verify` is only filled in when the
// "Verify after conversion" toggle was on for that run.
export interface ComparisonSummary {
  input_bytes: number;
  output_bytes: number;
  ratio_pct: number | null;
  input_format: string;
  output_format: string;
  output_sha1: string | null;
  verify: VerifyReport | null;
}

export interface RunOutcome {
  message: string;
  record: ReportRecord | null;
  input_bytes: number;
  output_bytes: number;
  comparison: ComparisonSummary | null;
}
