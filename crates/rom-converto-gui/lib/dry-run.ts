import type { OrganizeData, PlanDecision, PlanLine, RunPlansData } from "~/types";

// One row of a dry-run plan: source path, planned output, and the planner's
// note. Shared by the dry-run modal and the organize plan builder.
export interface DryRunLine {
	source: string;
	output: string;
	note: string;
	conflict?: boolean;
	// Nothing would happen for this line (skipped or already in place).
	muted?: boolean;
}

// The shapes a cmd_run dry run can carry: a single plan line, a batch of
// them, or organize's row list (whose rows carry their own conflicts).
type DryRunData = PlanLine | RunPlansData | OrganizeData;

// Decisions that touch an existing target count as conflicts; "New" writes a
// fresh file and keeps nothing.
function planConflicts(plans: PlanLine[]): boolean {
	return plans.some((plan) => plan.decision !== "New");
}

// Skip details that mean an existing output was kept, exact or as a prefix:
// the planner's two skip decisions and the on-conflict error refusals.
const PLAN_SKIP_DETAILS = ["output exists", "existing output verified valid", "output already exists", "output path exists"];

// True when a dry-run outcome would keep or replace an existing target, or
// when the run itself failed or partly failed.
export function outcomeConflicts(outcome: {
	status?: number;
	data?: unknown;
	records?: { status: string; error: string | null }[];
}): boolean {
	// Runner status: 0 = ok, anything else failed the run or parts of it.
	if (outcome.status !== undefined && outcome.status !== 0) return true;
	if (outcome.records?.some((record) => record.status === "failed")) return true;
	// cmd_run's payload is typed unknown across the IPC boundary; the `in`
	// checks below validate the shape before any member is read.
	const data = outcome.data as DryRunData | undefined;
	let planConflict = false;
	if (data) {
		if ("rows" in data) planConflict = false;
		else if ("decision" in data) planConflict = data.decision !== "New";
		else if ("plans" in data) planConflict = planConflicts(data.plans);
	}
	const skipConflict = !!outcome.records?.some((record) => {
		const error = record.error;
		return (
			record.status === "skipped"
			&& error !== null
			&& PLAN_SKIP_DETAILS.some((detail) => error === detail || error.startsWith(detail))
		);
	});
	return planConflict || skipConflict;
}

// One-line resolution text for a plan decision, in the planner's own terms.
export function decisionNote(decision: PlanDecision): string {
	if (decision === "Overwrite") return "Overwrite";
	if (decision === "KeepValid") return "Existing output verified valid";
	if (decision === "RewriteInvalid") return "Rewrite (invalid)";
	if (decision === "Skip") return "Skip (output exists)";
	if (typeof decision === "object") return `Rename to ${decision.Rename}`;
	return "";
}

// Summary note for a dry run: the decision that resolves the existing
// target for a single plan, the runner's message plus a conflict count for
// a batch, and the runner message otherwise (organize summarizes in its
// plan rows).
export function outcomeNote(outcome: { data?: unknown; message?: string }): string {
	const message = outcome.message ?? "";
	const data = outcome.data as DryRunData | undefined;
	if (data && "decision" in data) {
		const note = decisionNote(data.decision);
		if (note) return note;
	}
	if (data && "plans" in data) {
		const conflicts = data.plans.filter((plan) => plan.decision !== "New").length;
		if (conflicts) return `${message} (${conflicts} ${conflicts === 1 ? "conflict" : "conflicts"})`;
	}
	return message;
}
