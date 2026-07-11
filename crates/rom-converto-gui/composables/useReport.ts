import type { Ref } from "vue";
import { invoke } from "~/lib/ipc";
import type { ComparisonSummary, ReportRecord, ReportTotals, RunOutcome } from "~/types/report";

interface ReportableRefs {
  result: Ref<string>;
  error: Ref<string>;
  loading: Ref<boolean>;
  cancelled: Ref<boolean>;
}

// Read the input path from a command's args. Disc and NX commands name it
// `input`, the rest `inputPath`; both feed the same lib functions.
function inputOf(args: Record<string, unknown>): string {
  return String(args.input ?? args.inputPath ?? "");
}

// Synthesize a failed record for a file whose command threw, mirroring the
// CLI's `failed_record`: empty output, the input file size, and the error
// string. The lib still owns all report formatting; this only fills the fields
// the failing command could not return.
async function failedRecord(
  input: string,
  operation: string,
  error: string,
): Promise<ReportRecord> {
  let inputBytes = 0;
  try {
    inputBytes = await invoke<number>("cmd_file_size", { path: input });
  } catch {
    inputBytes = 0;
  }
  return {
    input_path: input,
    output_path: "",
    operation,
    status: "failed",
    input_bytes: inputBytes,
    output_bytes: 0,
    ratio_pct: null,
    elapsed_ms: 0,
    error,
  };
}

// Run a single-file report-capable command. The command returns a
// { message, record } object: the message drives the log and the record is
// pushed onto `records` for a later writeRunReport call. On failure a failed
// record is synthesized so the report matches the CLI, which records every
// processed file regardless of outcome.
export async function runReportable(
  command: string,
  args: Record<string, unknown>,
  refs: ReportableRefs,
  records: ReportRecord[],
  operation: string,
  comparisons?: ComparisonSummary[],
): Promise<void> {
  refs.result.value = "";
  refs.error.value = "";
  refs.cancelled.value = false;
  refs.loading.value = true;
  try {
    const res = await invoke<RunOutcome>(command, args);
    refs.result.value = res.message;
    if (res.record) records.push(res.record);
    if (comparisons && res.comparison) comparisons.push(res.comparison);
  } catch (e) {
    const message = String(e);
    if (message.includes("operation cancelled")) {
      refs.cancelled.value = true;
    } else {
      refs.error.value = message;
      if (args.report) {
        records.push(await failedRecord(inputOf(args), operation, message));
      }
    }
  } finally {
    refs.loading.value = false;
  }
}

// Append a failed record for a batch item whose command threw. Pages call this
// from the batch error hook so the report keeps a row per processed file.
export async function pushFailedRecord(
  records: ReportRecord[],
  input: string,
  operation: string,
  error: string,
): Promise<void> {
  records.push(await failedRecord(input, operation, error));
}

export function totalsFrom(records: ReportRecord[]): ReportTotals {
  const totals: ReportTotals = {
    total_files: records.length,
    ok: 0,
    skipped: 0,
    failed: 0,
    total_input_bytes: 0,
    total_output_bytes: 0,
    elapsed_ms: 0,
  };
  for (const r of records) {
    if (r.status === "ok") totals.ok++;
    else if (r.status === "skipped") totals.skipped++;
    else totals.failed++;
    totals.total_input_bytes += r.input_bytes;
    totals.total_output_bytes += r.output_bytes;
    totals.elapsed_ms += r.elapsed_ms;
  }
  return totals;
}

export async function writeRunReport(path: string, records: ReportRecord[]): Promise<void> {
  await invoke("cmd_write_report", {
    path,
    payload: { records, totals: totalsFrom(records) },
  });
}
