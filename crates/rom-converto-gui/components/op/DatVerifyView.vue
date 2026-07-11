<script setup lang="ts">
import { computed, ref } from "vue";
import { storeToRefs } from "pinia";
import { basename } from "~/composables/useDerivedPath";
import { useProgress } from "~/composables/useProgress";
import { useDatVerifyStore } from "~/stores/datVerify";
import { useQueueStore } from "~/stores/queue";
import type { StagedItem } from "~/lib/opdefs/types";
import ToggleSwitch from "~/components/ui/ToggleSwitch.vue";
import FilterChip from "~/components/ui/FilterChip.vue";
import StatusTag from "~/components/ui/StatusTag.vue";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";
import DetailModal from "~/components/modals/DetailModal.vue";
import DropZone from "~/components/op/DropZone.vue";
import StagedList from "~/components/op/StagedList.vue";

type Verdict = "verified" | "hint" | "unknown" | "unsupported" | "failed";

interface DatVerifyResult {
	kind: "verify";
	path: string;
	verdict: Verdict;
	matchAlgo: string | null;
	gameName: string | null;
	platform: string | null;
	signatureGroup: string | null;
	datFile: string | null;
	datFileId: string | null;
	datVersion: string | null;
	externalIds: { provider: string; id: string }[];
	tracks: { track: number; ok: boolean; algo: string | null; matchedFile: string | null }[] | null;
	error: string | null;
}

const store = useDatVerifyStore();
const { quick } = storeToRefs(store);
const queue = useQueueStore();
const progress = useProgress("dat-verify");

const staged = ref<StagedItem[]>([]);
const statusFilter = ref<Verdict | "all">("all");
const detailRow = ref<DatVerifyResult | null>(null);

const ROM_FILTERS = [{ name: "ROM file", extensions: ["*"] }];

function addPaths(paths: string[]) {
	for (const path of paths) {
		if (staged.value.some((s) => s.path === path)) continue;
		staged.value.push({ id: crypto.randomUUID(), path, name: basename(path), size: 0, outExt: "" });
	}
}

function removeStaged(id: string) {
	staged.value = staged.value.filter((s) => s.id !== id);
}

function clearStaged() {
	staged.value = [];
}

function enqueue() {
	if (!staged.value.length) return;
	queue.enqueue(
		staged.value.map((item) => ({
			name: item.name,
			opLabel: "dat verify",
			command: "cmd_dat_verify",
			args: { input: item.path, quick: quick.value },
			taskId: "dat-verify",
			chips: quick.value ? "quick" : "full",
			resultKind: "datVerify" as const,
			routeBack: { storeId: "dat-verify" },
		})),
	);
	clearStaged();
}

const results = computed<DatVerifyResult[]>(() => {
	const out: DatVerifyResult[] = [];
	for (const job of queue.finished) {
		if (job.command !== "cmd_dat_verify" || job.status !== "done" || typeof job.result !== "string") continue;
		try {
			out.push(JSON.parse(job.result) as DatVerifyResult);
		} catch {
			// non-JSON result, skip from the structured list
		}
	}
	return out;
});

type Chip = { verdict: Verdict; label: string; color: "green" | "yellow" | "neutral" | "red" };
const CHIPS: Chip[] = [
	{ verdict: "verified", label: "Verified", color: "green" },
	{ verdict: "hint", label: "Hint", color: "yellow" },
	{ verdict: "failed", label: "Failed", color: "red" },
	{ verdict: "unknown", label: "Unknown", color: "neutral" },
	{ verdict: "unsupported", label: "Unsupported", color: "neutral" },
];

const TAG: Record<Verdict, { tag: string; label: string }> = {
	verified: { tag: "VERIFIED", label: "Verified" },
	hint: { tag: "HINT", label: "Hint" },
	unknown: { tag: "UNKNOWN", label: "Unknown" },
	unsupported: { tag: "UNSUPPORTED", label: "Unsupported" },
	failed: { tag: "FAILED", label: "Failed" },
};

const counts = computed<Record<string, number>>(() => {
	const c: Record<string, number> = {};
	for (const r of results.value) c[r.verdict] = (c[r.verdict] ?? 0) + 1;
	return c;
});

const visibleRows = computed(() =>
	results.value.filter((r) => statusFilter.value === "all" || r.verdict === statusFilter.value),
);

function toggleFilter(verdict: Verdict) {
	statusFilter.value = statusFilter.value === verdict ? "all" : verdict;
}

function detail(r: DatVerifyResult): { text: string; tone: "green" | "red" | "muted" } | null {
	if (r.verdict === "failed") return { text: r.error ?? "Hash differs from the database entry.", tone: "red" };
	const text = [r.gameName, r.datFile].filter(Boolean).join(" · ");
	if (!text) return null;
	return { text, tone: r.verdict === "verified" ? "green" : "muted" };
}

function detailLines(r: DatVerifyResult): string[] {
	const lines: string[] = [];
	if (r.gameName) lines.push(`Game: ${r.gameName}`);
	if (r.datFile) lines.push(`DAT file: ${r.datFile}`);
	lines.push(r.error ?? "The full hash does not match the database entry. The file may be modified or corrupt.");
	return lines;
}
</script>

<template>
	<div class="rc-page">
		<div class="rc-head">
			<div class="rc-head__text">
				<h1 class="rc-head__title">Verify library</h1>
				<p class="rc-head__subtitle">
					Confirms each file's full hash matches its Playmatch DAT entry. Slower and stronger than a scan.
				</p>
			</div>
		</div>

		<DropZone
			drop-text="Drop ROM files or a folder to verify"
			:filters="ROM_FILTERS"
			:multiple="true"
			@add="addPaths"
		/>

		<StagedList
			v-if="staged.length"
			:items="staged"
			:label="`Staged: ${staged.length} files`"
			console-name="dat"
			@remove="removeStaged"
			@clear="clearStaged"
		/>

		<div class="rc-controls">
			<ToggleSwitch
				:model-value="quick"
				label="Quick verify"
				description="Trust a zip's own CRC32 for eligible cartridge images instead of extracting and hashing. Falls back automatically when that alone does not verify."
				@update:model-value="quick = $event"
			/>
		</div>

		<div class="rc-actions">
			<PrimaryButton :disabled="!staged.length" @click="enqueue">
				{{ staged.length ? `Add ${staged.length} to queue` : "Nothing staged" }}
			</PrimaryButton>
			<span class="rc-actions__note">Verify jobs run in the global queue. Results appear below as they finish.</span>
		</div>

		<div v-if="progress.running.value" class="rc-progress">
			<div class="rc-progress__track">
				<div class="rc-progress__fill" :style="{ width: `${progress.percent.value}%` }" />
			</div>
		</div>

		<div v-if="results.length" class="rc-chips">
			<FilterChip label="All" :count="results.length" :active="statusFilter === 'all'" @click="statusFilter = 'all'" />
			<FilterChip
				v-for="chip in CHIPS"
				:key="chip.verdict"
				:label="chip.label"
				:count="counts[chip.verdict] ?? 0"
				:color="chip.color"
				:active="statusFilter === chip.verdict"
				@click="toggleFilter(chip.verdict)"
			/>
		</div>

		<div v-if="results.length" class="rc-results">
			<div class="rc-results__head">Results</div>
			<div
				v-for="r in visibleRows"
				:key="r.path"
				class="rc-row"
				:class="{ 'rc-row--fail': r.verdict === 'failed' }"
			>
				<StatusTag :status="TAG[r.verdict].tag" :label="TAG[r.verdict].label" />
				<div class="rc-row__text">
					<span class="rc-row__name">{{ basename(r.path) }}</span>
					<span
						v-if="detail(r)"
						class="rc-row__detail"
						:class="`rc-row__detail--${detail(r)!.tone}`"
					>{{ detail(r)!.text }}</span>
				</div>
				<button v-if="r.verdict === 'failed'" type="button" class="rc-link" @click="detailRow = r">Details</button>
			</div>
		</div>

		<DetailModal
			v-if="detailRow"
			:title="basename(detailRow.path)"
			:lines="detailLines(detailRow)"
			@close="detailRow = null"
		/>
	</div>
</template>

<style scoped>
.rc-page {
	display: flex;
	flex-direction: column;
	gap: 14px;
	padding: 20px 26px;
}

.rc-head__title {
	margin: 0;
	font-size: 18px;
	font-weight: 700;
	color: var(--t0);
}

.rc-head__subtitle {
	margin: 4px 0 0;
	font-size: 11.5px;
	color: var(--t4);
	max-width: 560px;
	line-height: 1.45;
}

.rc-controls {
	display: flex;
	flex-direction: column;
	gap: 6px;
}

.rc-actions {
	display: flex;
	align-items: center;
	gap: 12px;
}

.rc-actions__note {
	font-size: 11.5px;
	color: var(--t4);
}

.rc-chips {
	display: flex;
	flex-wrap: wrap;
	gap: 8px;
}

.rc-results {
	border: 1px solid var(--a10);
	border-radius: 10px;
	background: var(--card);
	overflow: hidden;
}

.rc-results__head {
	padding: 10px 14px;
	border-bottom: 1px solid var(--a06);
	font-size: 10.5px;
	font-weight: 700;
	text-transform: uppercase;
	letter-spacing: 0.8px;
	color: var(--t4);
}

.rc-row {
	display: flex;
	align-items: center;
	gap: 12px;
	padding: 8px 14px;
	border-top: 1px solid var(--a06);
}

.rc-row--fail {
	background: rgba(212, 58, 62, 0.06);
}

.rc-row__text {
	display: flex;
	flex-direction: column;
	gap: 2px;
	min-width: 0;
	flex: 1;
}

.rc-row__name {
	color: var(--t0);
	font-size: 12px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-row__detail {
	font-size: 11px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-row__detail--green {
	color: var(--green);
}

.rc-row__detail--red {
	color: var(--red);
}

.rc-row__detail--muted {
	color: var(--t4);
}

.rc-link {
	border: none;
	background: none;
	color: var(--blue);
	font-size: 11.5px;
	cursor: pointer;
	padding: 0;
	white-space: nowrap;
}

.rc-progress {
	display: flex;
	align-items: center;
	gap: 10px;
}

.rc-progress__track {
	flex: 1;
	height: 4px;
	border-radius: 3px;
	background: var(--a10);
	overflow: hidden;
}

.rc-progress__fill {
	height: 100%;
	background: #2f6fd0;
	transition: width 0.15s linear;
}
</style>
