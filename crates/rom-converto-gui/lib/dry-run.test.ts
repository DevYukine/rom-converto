import { describe, expect, it } from "vitest";
import { decisionNote, outcomeConflicts, outcomeNote } from "./dry-run";
import type { OrganizeData, PlanDecision, PlanLine, RunPlansData } from "~/types";

function plan(decision: PlanDecision): PlanLine {
	return { operation: "chd.compress", input: "/lib/a.iso", output: "/lib/a.chd", decision, media: null, missing_keys: null };
}

const organize: OrganizeData = { rows: [], dry_run: true, ok: 0, skipped: 2, failed: 0, playlists: [] };

function plans(...decisions: PlanDecision[]): RunPlansData {
	return { plans: decisions.map(plan) };
}

const skippedExisting = { status: "skipped", error: "output exists" };
const skippedValid = { status: "skipped", error: "existing output verified valid" };
const skippedOther = { status: "skipped", error: "already in place" };
const skippedRefused = { status: "skipped", error: "output already exists: /lib/a.chd; pass --on-conflict overwrite to replace it" };
const okRecord = { status: "ok", error: null };
const failedRecord = { status: "failed", error: "io error" };

describe("outcomeConflicts", () => {
	it("flags every decision that touches an existing target", () => {
		expect(outcomeConflicts({ data: plan("New") })).toBe(false);
		expect(outcomeConflicts({ data: plan("Overwrite") })).toBe(true);
		expect(outcomeConflicts({ data: plan({ Rename: "/lib/b.chd" }) })).toBe(true);
		expect(outcomeConflicts({ data: plan("Skip") })).toBe(true);
		expect(outcomeConflicts({ data: plan("KeepValid") })).toBe(true);
		expect(outcomeConflicts({ data: plan("RewriteInvalid") })).toBe(true);
	});

	it("reads batch plan lists and ignores organize rows", () => {
		expect(outcomeConflicts({ data: plans("New", "New") })).toBe(false);
		expect(outcomeConflicts({ data: plans("New", "Overwrite") })).toBe(true);
		expect(outcomeConflicts({ data: organize, records: [skippedOther, okRecord] })).toBe(false);
	});

	it("counts only the runner's plan skips among skipped records", () => {
		expect(outcomeConflicts({ records: [skippedExisting] })).toBe(true);
		expect(outcomeConflicts({ records: [skippedValid] })).toBe(true);
		expect(outcomeConflicts({ records: [skippedRefused] })).toBe(true);
		expect(outcomeConflicts({ records: [skippedOther] })).toBe(false);
		expect(outcomeConflicts({ records: [okRecord] })).toBe(false);
		expect(outcomeConflicts({})).toBe(false);
	});

	it("flags failed runs and failed records, and keeps plan checks beside records", () => {
		expect(outcomeConflicts({ status: 0, data: plans("New") })).toBe(false);
		expect(outcomeConflicts({ status: 3, data: plans("New") })).toBe(true);
		expect(outcomeConflicts({ records: [failedRecord] })).toBe(true);
		expect(outcomeConflicts({ status: 0, data: plans("New"), records: [skippedRefused] })).toBe(true);
	});
});

describe("decisionNote", () => {
	it("names the resolution in the planner's terms", () => {
		expect(decisionNote("New")).toBe("");
		expect(decisionNote("Overwrite")).toBe("Overwrite");
		expect(decisionNote({ Rename: "/lib/b.chd" })).toBe("Rename to /lib/b.chd");
		expect(decisionNote("KeepValid")).toBe("Existing output verified valid");
		expect(decisionNote("RewriteInvalid")).toBe("Rewrite (invalid)");
		expect(decisionNote("Skip")).toBe("Skip (output exists)");
	});
});

describe("outcomeNote", () => {
	it("prefers the planned resolution for a single plan line", () => {
		const message = { message: "Dry run planned." };
		expect(outcomeNote({ ...message, data: plan("Overwrite") })).toBe("Overwrite");
		expect(outcomeNote({ ...message, data: plan({ Rename: "/lib/b.chd" }) })).toBe("Rename to /lib/b.chd");
		expect(outcomeNote({ ...message, data: plan("New") })).toBe("Dry run planned.");
		expect(outcomeNote(message)).toBe("Dry run planned.");
		expect(outcomeNote({})).toBe("");
	});

	it("keeps the batch message and appends a conflict count", () => {
		const batch = { message: "3 files completed (2 ok, 1 skipped)." };
		expect(outcomeNote({ ...batch, data: plans("New", "New", "Overwrite") })).toBe(
			"3 files completed (2 ok, 1 skipped). (1 conflict)",
		);
		expect(outcomeNote({ ...batch, data: plans("RewriteInvalid", { Rename: "/lib/b.chd" }) })).toBe(
			"3 files completed (2 ok, 1 skipped). (2 conflicts)",
		);
		expect(outcomeNote({ ...batch, data: plans("New") })).toBe("3 files completed (2 ok, 1 skipped).");
		expect(outcomeNote({ ...batch, data: organize })).toBe("3 files completed (2 ok, 1 skipped).");
	});
});
