import { defineStore } from "pinia";
import { invoke } from "~/lib/ipc";
import type { RunOutcome } from "~/types/report";
import { useUiStore } from "~/stores/ui";
import { useAlertsStore } from "~/stores/alerts";
import { useJobConcurrency } from "~/composables/useJobConcurrency";
import { useProgress } from "~/composables/useProgress";
import { useBatchNotify } from "~/composables/useBatchNotify";
import { pushFailedRecord, writeRunReport } from "~/composables/useReport";
import type { ReportRecord } from "~/types/report";

export type JobStatus = "queued" | "running" | "done" | "failed" | "cancelled";
export type ResultKind = "convert" | "verify" | "hash" | "datVerify" | "text";

export interface QueueJob {
	id: string;
	name: string;
	opLabel: string;
	command: string;
	args: Record<string, unknown>;
	taskId: string;
	chips: string;
	status: JobStatus;
	resultKind: ResultKind;
	routeBack?: { storeId: string };
	groupId?: string;
	result?: RunOutcome | string;
	error?: string;
	inputBytes: number;
	outputBytes: number;
	startedAt?: number;
}

export interface EnqueueJob {
	name: string;
	opLabel: string;
	command: string;
	args: Record<string, unknown>;
	taskId: string;
	chips: string;
	resultKind: ResultKind;
	routeBack?: { storeId: string };
	groupId?: string;
	inputBytes?: number;
}

const GIB = 2 ** 30;

function isActive(s: JobStatus): boolean {
	return s === "queued" || s === "running";
}

function inputOf(job: QueueJob): string {
	return String(job.args.input ?? job.args.inputPath ?? job.name);
}

export const useQueueStore = defineStore("queue", () => {
	const ui = useUiStore();
	const alerts = useAlertsStore();
	const { concurrency } = useJobConcurrency();
	const notify = useBatchNotify();

	const jobs = ref<QueueJob[]>([]);
	const queueActive = ref(false);
	const drawerOpen = ref(false);

	let batchDone = 0;
	let batchFailed = 0;

	const running = computed(() => jobs.value.filter((j) => j.status === "running"));
	const queued = computed(() => jobs.value.filter((j) => j.status === "queued"));
	const done = computed(() => jobs.value.filter((j) => j.status === "done"));
	const failed = computed(() => jobs.value.filter((j) => j.status === "failed"));
	const finished = computed(() =>
		jobs.value.filter(
			(j) => j.status === "done" || j.status === "failed" || j.status === "cancelled",
		),
	);

	const counts = computed(() => ({
		running: running.value.length,
		queued: queued.value.length,
		done: done.value.length,
		failed: failed.value.length,
	}));

	const avgRunningPct = computed(() => {
		const r = running.value;
		if (!r.length) return 0;
		let sum = 0;
		for (const j of r) sum += useProgress(j.taskId).percent.value;
		return Math.round(sum / r.length);
	});

	const savedBytes = computed(() =>
		done.value
			.filter((j) => j.resultKind === "convert")
			.reduce((acc, j) => acc + Math.max(0, j.inputBytes - j.outputBytes), 0),
	);
	const savedGiB = computed(() => (savedBytes.value / GIB).toFixed(1));

	const speedBps = ref(0);
	let prevBytes = 0;
	let prevT = 0;
	let sampleTimer: ReturnType<typeof setInterval> | null = null;

	function runningBytes(): number {
		let cur = 0;
		for (const j of running.value) cur += useProgress(j.taskId).current.value;
		return cur;
	}

	function sample() {
		const now = performance.now();
		const cur = runningBytes();
		if (prevT) {
			const dt = (now - prevT) / 1000;
			if (dt > 0) speedBps.value = Math.max(0, (cur - prevBytes) / dt);
		}
		prevBytes = cur;
		prevT = now;
	}

	function stopSampler() {
		if (sampleTimer) clearInterval(sampleTimer);
		sampleTimer = null;
		speedBps.value = 0;
		prevBytes = 0;
		prevT = 0;
	}

	if (typeof window !== "undefined") {
		watch(
			() => running.value.length,
			(n) => {
				if (n > 0 && !sampleTimer) {
					prevT = 0;
					sampleTimer = setInterval(sample, 1000);
				} else if (n === 0) {
					stopSampler();
				}
			},
		);
		watch([avgRunningPct, running], () => {
			if (!ui.taskbarProgress) return;
			if (running.value.length) notify.setTaskbarProgress(avgRunningPct.value / 100);
		});
	}

	const etaMin = computed<number | null>(() => {
		if (speedBps.value <= 0) return null;
		let remaining = 0;
		for (const j of running.value) {
			const p = useProgress(j.taskId);
			remaining += Math.max(0, p.total.value - p.current.value);
		}
		if (remaining <= 0) return null;
		return Math.max(1, Math.round(remaining / speedBps.value / 60));
	});

	const statusText = computed(() => {
		if (!running.value.length) return "idle";
		const mb = (speedBps.value / 1e6).toFixed(1);
		return etaMin.value != null ? `${mb} MB/s · ETA ${etaMin.value} min` : `${mb} MB/s`;
	});

	function byId(id: string): QueueJob | undefined {
		return jobs.value.find((j) => j.id === id);
	}

	function enqueue(specs: EnqueueJob[]): QueueJob[] {
		const created: QueueJob[] = [];
		for (const s of specs) {
			const job: QueueJob = {
				id: crypto.randomUUID(),
				name: s.name,
				opLabel: s.opLabel,
				command: s.command,
				args: Object.freeze({ ...s.args }),
				taskId: s.taskId,
				chips: s.chips,
				status: "queued",
				resultKind: s.resultKind,
				routeBack: s.routeBack,
				groupId: s.groupId,
				inputBytes: s.inputBytes ?? 0,
				outputBytes: 0,
			};
			jobs.value.push(job);
			created.push(job);
		}
		pump();
		return created;
	}

	function pump() {
		if (!ui.startImmediately && !queueActive.value) return;
		const keys = new Set(running.value.map((j) => j.taskId));
		for (const job of jobs.value) {
			if (running.value.length >= concurrency.value) break;
			if (job.status !== "queued") continue;
			if (keys.has(job.taskId)) continue;
			keys.add(job.taskId);
			void startJob(job);
		}
	}

	async function startJob(job: QueueJob) {
		job.status = "running";
		job.startedAt = Date.now();
		useProgress(job.taskId).reset();
		try {
			const res = await invoke<RunOutcome>(job.command, job.args as Record<string, unknown>);
			settle(job, res, null);
		} catch (e) {
			settle(job, null, String(e));
		}
	}

	function settle(job: QueueJob, res: RunOutcome | null, err: string | null) {
		if (job.status === "cancelled") {
			afterSettle();
			return;
		}
		if (err !== null) {
			if (err.includes("operation cancelled")) {
				job.status = "cancelled";
			} else {
				job.status = "failed";
				job.error = err;
				batchFailed++;
				alerts.push({
					type: "error",
					title: "Conversion failed",
					body: `${job.name}: ${err}`,
					actions: [
						{ label: "Retry", run: () => retry(job.id) },
						{ label: "Show in queue", run: () => void navigateTo("/queue") },
					],
					meta: `${job.opLabel} · just now`,
				});
			}
		} else if (res) {
			job.status = "done";
			job.result = res;
			job.outputBytes = res.output_bytes ?? 0;
			if (res.input_bytes) job.inputBytes = res.input_bytes;
			batchDone++;
		} else {
			job.status = "done";
			batchDone++;
		}
		void maybeWriteReport(job);
		afterSettle();
	}

	function afterSettle() {
		pump();
		if (running.value.length === 0 && queued.value.length === 0) drain();
	}

	async function maybeWriteReport(job: QueueJob) {
		if (!job.groupId) return;
		const reportFile = job.args.reportFile as string | null | undefined;
		if (!reportFile) return;
		const group = jobs.value.filter((j) => j.groupId === job.groupId);
		if (group.some((j) => isActive(j.status))) return;
		const records: ReportRecord[] = [];
		for (const j of group) {
			if (j.status === "done" && j.result && typeof j.result !== "string") {
				if (j.result.record) records.push(j.result.record);
			} else if (j.status === "failed") {
				await pushFailedRecord(records, inputOf(j), j.opLabel, j.error ?? "failed");
			}
		}
		if (!records.length) return;
		try {
			await writeRunReport(reportFile, records);
		} catch {
			// Report writing is best-effort; a failure here must not break the queue.
		}
	}

	function drain() {
		const doneN = batchDone;
		const failedN = batchFailed;
		batchDone = 0;
		batchFailed = 0;
		const total = doneN + failedN;
		if (total === 0) return;
		const anyFail = failedN > 0;
		if (ui.taskbarProgress) {
			if (anyFail) notify.taskbarError();
			else notify.clearTaskbar();
		}
		// Lone successful job stays silent; only batches (2+) or any failure surface.
		if (total < 2 && !anyFail) return;
		const body = `${doneN} of ${total} done, ${failedN} failed · ${savedGiB.value} GiB saved`;
		notify.notifyBatchDone("Batch finished", body, ui.soundEnabled);
		// A lone failure already raised its own error alert; no batch summary on top.
		if (total < 2) return;
		alerts.push({
			type: "ok",
			title: "Batch finished",
			body,
			actions: [{ label: "Show in queue", run: () => void navigateTo("/queue") }],
			meta: "just now",
		});
	}

	function cancelFailed(body: string) {
		alerts.push({
			type: "error",
			title: "Cancel failed",
			body,
			meta: "just now",
		});
	}

	function cancel(id: string) {
		const job = byId(id);
		if (!job) return;
		if (job.status === "queued") {
			job.status = "cancelled";
			afterSettle();
		} else if (job.status === "running") {
			invoke("cmd_cancel", { taskId: job.taskId })
				.then(() => {
					job.status = "cancelled";
					afterSettle();
				})
				.catch((e) => cancelFailed(`${job.name}: ${String(e)}`));
		}
	}

	function cancelAll() {
		invoke("cmd_cancel", {})
			.then(() => {
				for (const j of jobs.value) {
					if (isActive(j.status)) j.status = "cancelled";
				}
			})
			.catch((e) => cancelFailed(String(e)));
	}

	function retry(id: string) {
		const job = byId(id);
		if (!job || job.status !== "failed") return;
		enqueue([
			{
				name: job.name,
				opLabel: job.opLabel,
				command: job.command,
				args: { ...job.args },
				taskId: job.taskId,
				chips: job.chips,
				resultKind: job.resultKind,
				routeBack: job.routeBack,
				groupId: job.groupId,
				inputBytes: job.inputBytes,
			},
		]);
		remove(id);
	}

	function retryFailed() {
		for (const j of [...failed.value]) retry(j.id);
	}

	function clearFinished() {
		jobs.value = jobs.value.filter((j) => isActive(j.status));
	}

	function remove(id: string) {
		jobs.value = jobs.value.filter((j) => j.id !== id);
	}

	function reorder(ids: string[]) {
		const order = new Map(ids.map((id, i) => [id, i]));
		jobs.value = [...jobs.value].sort((a, b) => {
			if (a.status === "queued" && b.status === "queued") {
				return (order.get(a.id) ?? 0) - (order.get(b.id) ?? 0);
			}
			return 0;
		});
	}

	function start() {
		queueActive.value = true;
		pump();
	}

	function pause() {
		queueActive.value = false;
	}

	return {
		jobs,
		queueActive,
		drawerOpen,
		running,
		queued,
		done,
		failed,
		finished,
		counts,
		avgRunningPct,
		savedBytes,
		savedGiB,
		statusText,
		etaMin,
		speedBps,
		enqueue,
		pump,
		cancel,
		cancelAll,
		retry,
		retryFailed,
		clearFinished,
		remove,
		reorder,
		start,
		pause,
	};
});
