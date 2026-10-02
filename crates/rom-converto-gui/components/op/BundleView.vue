<script setup lang="ts">
import { computed, ref } from "vue";
import { invoke, open, save } from "~/lib/ipc";
import { basename, deriveWuaPath } from "~/composables/useDerivedPath";
import { formatBytes } from "~/lib/inspect/shared";
import { buildCliCommand } from "~/composables/useCliEcho";
import { useQueueStore } from "~/stores/queue";
import { isDiscInput } from "~/stores/wup-compress";
import CliChip from "~/components/ui/CliChip.vue";
import DropZone from "~/components/op/DropZone.vue";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import LevelSlider from "~/components/ui/LevelSlider.vue";
import ToggleSwitch from "~/components/ui/ToggleSwitch.vue";
import ConflictPopover from "~/components/modals/ConflictPopover.vue";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";
import DryRunModal from "~/components/modals/DryRunModal.vue";
import type { DryRunLine } from "~/components/modals/DryRunModal.vue";
import type { InfoResult, WupInfo } from "~/types";
import { dryRunArgs, opCommand, opProgressKey, runArgs } from "~/lib/opdefs/types";
import type { OpDef } from "~/lib/opdefs/types";

const props = defineProps<{ def: OpDef }>();

const store = props.def.useStore();
const queue = useQueueStore();
const { show: showToast } = useToast();

type PartKind = "base" | "update" | "dlc" | "unknown";

interface Part {
	id: string;
	path: string;
	isDisc: boolean;
	key: string;
	name: string;
	titleIdHex: string;
	version: number;
	size: number;
	kind: PartKind;
	lowId: string;
	error: string;
}

const parts = ref<Part[]>([]);

function classify(hex: string): PartKind {
	const hi = hex.slice(0, 8).toLowerCase();
	if (hi === "00050000") return "base";
	if (hi === "0005000e") return "update";
	if (hi === "0005000c") return "dlc";
	return "unknown";
}

function wupName(info: WupInfo): string {
	return info.meta?.long_names?.entries?.[0]?.[1] || info.title_id_hex;
}

async function probe(part: Part) {
	part.error = "";
	try {
		const json = await invoke<string>("cmd_read_info", {
			input: part.path,
			keys: part.key || null,
		});
		const info = JSON.parse(json) as InfoResult;
		if (info.kind !== "wup") {
			part.error = "Not a Wii U title";
			return;
		}
		part.name = wupName(info);
		part.titleIdHex = info.title_id_hex;
		part.version = info.title_version;
		part.size = info.total_content_size;
		part.kind = classify(info.title_id_hex);
		part.lowId = info.title_id_hex.slice(-8).toUpperCase();
	} catch (e) {
		part.error = String(e);
	}
}

function add(paths: string[]) {
	for (const path of paths) {
		if (parts.value.some((p) => p.path === path)) continue;
		const part: Part = {
			id: crypto.randomUUID(),
			path,
			isDisc: isDiscInput(path),
			key: "",
			name: basename(path),
			titleIdHex: "",
			version: 0,
			size: 0,
			kind: "unknown",
			lowId: "",
			error: "",
		};
		parts.value.push(part);
		void probe(parts.value[parts.value.length - 1]!);
	}
	queued.value = false;
}

function removePart(id: string) {
	parts.value = parts.value.filter((p) => p.id !== id);
	queued.value = false;
}

async function browseFolder() {
	const picked = await open({ directory: true, multiple: true });
	if (Array.isArray(picked)) add(picked);
	else if (typeof picked === "string") add([picked]);
}

async function pickKey(part: Part) {
	const picked = await open({ multiple: false });
	if (typeof picked === "string") {
		part.key = picked;
		await probe(part);
	}
}

const KIND_ORDER: Record<PartKind, number> = { base: 0, update: 1, dlc: 2, unknown: 3 };

interface Bundle {
	lowId: string;
	parts: Part[];
	base: Part | null;
	complete: boolean;
}

const bundles = computed<Bundle[]>(() => {
	const map = new Map<string, Part[]>();
	for (const p of parts.value) {
		if (!p.titleIdHex) continue;
		const arr = map.get(p.lowId) ?? [];
		arr.push(p);
		map.set(p.lowId, arr);
	}
	return [...map.entries()].map(([lowId, ps]) => {
		const ordered = [...ps].sort((a, b) => KIND_ORDER[a.kind] - KIND_ORDER[b.kind]);
		const base = ordered.find((p) => p.kind === "base") ?? null;
		return { lowId, parts: ordered, base, complete: !!base };
	});
});

// Parts still resolving: reading, or a failed probe.
const unresolved = computed(() => parts.value.filter((p) => !p.titleIdHex));

const readyBundles = computed(() => bundles.value.filter((b) => b.complete));

function badge(b: Bundle): string {
	if (!b.complete) return "";
	const hasU = b.parts.some((p) => p.kind === "update");
	const hasD = b.parts.some((p) => p.kind === "dlc");
	if (hasU && hasD) return "✓ base + update + DLC";
	if (hasU) return "✓ base + update";
	if (hasD) return "✓ base + DLC";
	return "✓ base only";
}

function bundleTotal(b: Bundle): number {
	return b.parts.reduce((n, p) => n + p.size, 0);
}

function bundleName(b: Bundle): string {
	return b.base ? b.base.name : "No matching base game";
}

// Per-bundle output override, keyed by lowId. Falls back to the derived
// path next to the base title until the user picks one explicitly.
const outputOverrides = ref<Record<string, string>>({});

function bundleOutput(b: Bundle): string {
	return outputOverrides.value[b.lowId] || deriveWuaPath((b.base as Part).path);
}

async function pickOutput(b: Bundle) {
	const picked = await save({
		filters: [{ name: "Wii U Archive", extensions: ["wua"] }],
		defaultPath: bundleOutput(b),
	});
	if (typeof picked === "string") outputOverrides.value[b.lowId] = picked;
}

function bundleArgs(b: Bundle, taskId = newTaskId()) {
	return runArgs(
		"wup.compress",
		null,
		bundleOutput(b),
		{
			level: store.level,
			inputs: b.parts.map((p) => ({
				path: p.path,
				format: p.isDisc ? "disc" : null,
				key: p.key || null,
				key_path: null,
			})),
			on_conflict: store.onConflict,
			skip_space_check: store.skipSpaceCheck,
		},
		false,
		taskId,
	);
}

// Unique per bundle, so cancelling one bundle leaves the others running.
function newTaskId(): string {
	return `job-${crypto.randomUUID()}`;
}

function progressKey(): string | undefined {
	return opProgressKey(props.def, store);
}

const partTag: Record<PartKind, string> = { base: "Base", update: "Update", dlc: "DLC", unknown: "?" };

const cli = computed(() => {
	const b = readyBundles.value[0];
	return buildCliCommand(
		b ? bundleArgs(b) : runArgs("wup.compress", "input.wud", null, { level: store.level }, false, newTaskId()),
	);
});

const queued = ref(false);
const addLabel = computed(() =>
	queued.value ? "Bundles queued ✓" : readyBundles.value.length === 0 ? (parts.value.length === 0 ? "Nothing staged" : "No complete bundles") : `Add ${readyBundles.value.length} bundle${readyBundles.value.length === 1 ? "" : "s"} to queue`,
);

function addBundles() {
	if (queued.value || !readyBundles.value.length) return;
	const specs = readyBundles.value.map((b) => {
		const taskId = newTaskId();
		return {
			name: basename(bundleOutput(b)),
			opLabel: props.def.opLabel,
			command: opCommand(props.def, store),
			args: bundleArgs(b, taskId),
			taskId,
			progressKey: progressKey(),
			chips: `level ${store.level}`,
			resultKind: props.def.resultKind,
			routeBack: { storeId: props.def.storeId },
			inputBytes: bundleTotal(b),
		};
	});
	queue.enqueue(specs);
	queued.value = true;
}

const dryLines = ref<DryRunLine[]>([]);
const dryCommand = ref("");
const dryOpen = ref(false);

async function dryRun() {
	if (!readyBundles.value.length) return;
	const lines: DryRunLine[] = [];
	let cmd = "";
	for (const b of readyBundles.value) {
		const args = bundleArgs(b);
		if (!cmd) cmd = buildCliCommand(args);
		let note = "ok";
		let conflict = false;
		try {
			const command = opCommand(props.def, store);
			const res = await invoke<{ message?: string }>(command, dryRunArgs(args));
			const msg = typeof res === "object" && res ? String(res.message ?? "") : String(res);
			if (msg) note = msg;
			conflict = /exists|rename/i.test(msg);
		} catch (e) {
			note = String(e);
			conflict = true;
		}
		lines.push({ source: bundleName(b), output: bundleOutput(b), note, conflict });
	}
	dryLines.value = lines;
	dryCommand.value = cmd;
	dryOpen.value = true;
}

function copied() {
	showToast("Copied");
}

</script>

<template>
	<div class="rc-page">
		<div class="rc-head">
			<div class="rc-head__text">
				<h1 class="rc-head__title">{{ def.title }}</h1>
				<p class="rc-head__subtitle">{{ def.subtitle }}</p>
			</div>
			<CliChip :command="cli" @copy="copied" />
		</div>

		<DropZone
			:drop-text="def.dropText"
			:filters="def.browseFilters"
			file-label="Browse disc image"
			multiple
			also-directory
			@add="add"
		/>

		<div v-if="unresolved.length" class="rc-card rc-card--warn">
			<div class="rc-card__head">Unreadable or key not auto-detected</div>
			<div v-for="p in unresolved" :key="p.id" class="rc-part">
				<span class="rc-tag rc-tag--unknown">?</span>
				<span class="rc-part__name" :title="p.name">{{ p.name }}</span>
				<span class="rc-part__meta" :title="p.error || 'reading…'">{{ p.error || "reading…" }}</span>
				<button v-if="p.isDisc" type="button" class="rc-link" @click="pickKey(p)">
					{{ p.key ? "Change key…" : "Master key…" }}
				</button>
				<span v-else />
				<button type="button" class="rc-x" @click="removePart(p.id)">✕</button>
			</div>
		</div>

		<div
			v-for="b in bundles"
			:key="b.lowId"
			class="rc-card"
			:class="{ 'rc-card--warn': !b.complete }"
		>
			<div class="rc-bundle__head">
				<span class="rc-bundle__title" :class="{ 'rc-bundle__title--warn': !b.complete }" :title="bundleName(b)">
					{{ bundleName(b) }}
				</span>
				<span v-if="b.complete" class="rc-bundle__badge">{{ badge(b) }}</span>
				<span v-else class="rc-bundle__note">
					Update or DLC without its base game. Add the base title (00050000{{ b.lowId }}) to bundle it.
				</span>
				<span v-if="b.complete" class="rc-bundle__total">
					Total {{ formatBytes(bundleTotal(b)) }}
				</span>
				<button v-if="!b.complete" type="button" class="rc-link" @click="browseFolder">Locate base…</button>
			</div>
			<div v-if="b.complete" class="rc-output-row">
				<FieldLabel label="Output" />
				<button type="button" class="rc-picker rc-bundle__output" :title="bundleOutput(b)" :aria-label="`Output: ${bundleOutput(b)}`" @click="pickOutput(b)">
					<span class="rc-picker__value">{{ bundleOutput(b) }}</span>
					<svg class="rc-picker__icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true">
						<path d="M3 7h6l2 2h10v10H3z" />
					</svg>
				</button>
			</div>
			<div v-for="p in b.parts" :key="p.id" class="rc-part">
				<span class="rc-tag" :class="`rc-tag--${p.kind}`">{{ partTag[p.kind] }}</span>
				<span class="rc-part__name" :title="p.name">{{ p.name }}</span>
				<span class="rc-part__meta" :title="`${p.titleIdHex} · v${p.version} · ${formatBytes(p.size)}`">{{ p.titleIdHex }} · v{{ p.version }} · {{ formatBytes(p.size) }}</span>
				<button v-if="p.isDisc" type="button" class="rc-link" @click="pickKey(p)">
					{{ p.key ? "Master key ✓" : "Master key…" }}
				</button>
				<span v-else />
				<button type="button" class="rc-x" @click="removePart(p.id)">✕</button>
			</div>
		</div>

		<div class="rc-grid">
			<ConfigCard title="Compression">
				<LevelSlider
					:model-value="store.level"
					:min="0"
					:max="22"
					label="Zstd level"
					hint="0 uses Cemu's default (6). 1 is fastest, 22 is max ratio."
					tooltip="Zstd compression level. 0 means the Cemu default of 6. Higher levels produce smaller output at the cost of compression time."
					:format-value="(v) => (v === 0 ? 'default (0)' : String(v))"
					@update:model-value="store.level = $event"
				/>
			</ConfigCard>

			<ConfigCard title="Safety">
				<div class="rc-conflict-row">
					<FieldLabel
						label="On conflict"
						tooltip="What to do when the output file already exists. The choice is resolved before anything is written."
					/>
					<ConflictPopover
						:model-value="store.onConflict"
						@update:model-value="store.onConflict = $event"
					/>
				</div>
				<ToggleSwitch
					:model-value="store.skipSpaceCheck"
					label="Skip free-space check"
					tooltip="Skips the free space estimate taken before writing. Use it only when the estimate is wrong for your disk."
					@update:model-value="store.skipSpaceCheck = $event"
				/>
			</ConfigCard>
		</div>

		<div class="rc-actions">
			<PrimaryButton :disabled="queued || readyBundles.length === 0" @click="addBundles">
				{{ addLabel }}
			</PrimaryButton>
			<PrimaryButton
				variant="outlined"
				:disabled="readyBundles.length === 0"
				@click="dryRun"
			>
				Dry run
			</PrimaryButton>
			<span class="rc-actions__note">{{ def.actionNote }}</span>
		</div>

		<DryRunModal v-if="dryOpen" :command="dryCommand" :lines="dryLines" @close="dryOpen = false" />
	</div>
</template>

<style scoped>
.rc-page {
	display: flex;
	flex-direction: column;
	gap: 16px;
	padding: 24px 28px 32px;
}

.rc-head {
	display: flex;
	flex-wrap: wrap;
	align-items: flex-start;
	gap: 10px 24px;
}

.rc-head__text {
	flex: 1 1 480px;
	min-width: 0;
}

.rc-head > :deep(.rc-cli-chip) {
	flex: 0 1 auto;
	min-width: 0;
}

.rc-head__title {
	margin: 0;
	font-size: var(--fs-xl);
	line-height: 1.25;
	font-weight: 700;
	color: var(--t0);
	text-wrap: balance;
}

.rc-head__subtitle {
	margin: 4px 0 0;
	max-width: 72ch;
	font-size: var(--fs-md);
	line-height: var(--lh-body);
	color: var(--t4);
	text-wrap: pretty;
}

.rc-card {
	min-width: 0;
	container: bundle / inline-size;
	border: 1px solid var(--a10);
	border-radius: var(--r-lg);
	background: var(--card);
	padding: 14px 16px;
	display: flex;
	flex-direction: column;
}

.rc-card--warn {
	border-color: var(--yellow);
}

.rc-card__head {
	margin-bottom: 8px;
	font-size: var(--fs-lg);
	font-weight: 600;
	color: var(--yellow);
}

.rc-bundle__head {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 8px 16px;
	margin-bottom: 8px;
}

.rc-bundle__title {
	flex: 1 1 240px;
	min-width: 0;
	font-weight: 600;
	color: var(--t1);
	font-size: var(--fs-lg);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-bundle__title--warn {
	color: var(--yellow);
}

.rc-bundle__badge {
	flex: none;
	font-size: var(--fs-xs);
	color: var(--green);
	background: var(--tint-green);
	border-radius: var(--r-sm);
	padding: 2px 7px;
	white-space: nowrap;
}

.rc-bundle__note {
	flex: 1 1 260px;
	min-width: 0;
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	color: var(--t5);
	text-wrap: pretty;
}

.rc-bundle__total {
	flex: none;
	font-family: var(--font-mono);
	font-size: var(--fs-xs);
	color: var(--t5);
	white-space: nowrap;
}

.rc-bundle__output {
	width: 100%;
}

.rc-bundle__output .rc-picker__value {
	font-family: var(--font-mono);
}

.rc-output-row {
	display: flex;
	flex-direction: column;
	gap: 6px;
	padding: 6px 0 12px;
}

.rc-part {
	display: grid;
	grid-template-columns: 60px minmax(0, 1fr) minmax(0, 1.4fr) 104px var(--ctl-h);
	align-items: center;
	gap: 6px 10px;
	min-height: 40px;
	padding: 6px 0;
	border-top: 1px solid var(--a06);
}

.rc-tag {
	flex: none;
	width: 60px;
	text-align: center;
	border-radius: var(--r-sm);
	padding: 2px 0;
	font-size: var(--fs-xs);
	font-weight: 600;
	white-space: nowrap;
}

.rc-tag--base {
	background: var(--tint-blue);
	color: var(--blue);
}

.rc-tag--update {
	background: var(--tint-green);
	color: var(--green);
}

.rc-tag--dlc {
	background: var(--tint-yellow);
	color: var(--yellow);
}

.rc-tag--unknown {
	background: var(--a08);
	color: var(--t5);
}

.rc-part__name {
	min-width: 0;
	color: var(--t0);
	font-size: var(--fs-md);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-part__meta {
	min-width: 0;
	font-family: var(--font-mono);
	font-size: var(--fs-xs);
	color: var(--t5);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

@container bundle (max-width: 639px) {
	.rc-part {
		grid-template-columns: 60px minmax(0, 1fr) 104px var(--ctl-h);
	}

	.rc-part__meta {
		grid-column: 2 / -1;
		grid-row: 2;
	}
}

.rc-link {
	flex: none;
	border: none;
	border-radius: var(--r-sm);
	background: transparent;
	color: var(--blue);
	font-size: var(--fs-sm);
	min-height: var(--ctl-h);
	padding: 0 8px;
	cursor: pointer;
	white-space: nowrap;
}

.rc-link:hover {
	background: var(--a06);
}

.rc-x {
	flex: none;
	width: var(--ctl-h);
	height: var(--ctl-h);
	border: none;
	border-radius: var(--r-sm);
	background: transparent;
	color: var(--t5);
	cursor: pointer;
	font-size: var(--fs-md);
	white-space: nowrap;
}

.rc-x:hover {
	color: var(--red);
	background: var(--tint-red);
}

.rc-grid {
	display: grid;
	grid-template-columns: minmax(0, 1fr);
	align-items: start;
	gap: 16px;
}

@container page (min-width: 820px) {
	.rc-grid {
		grid-template-columns: minmax(0, 1fr) minmax(280px, 340px);
	}

	.rc-grid > :last-child {
		position: sticky;
		top: 16px;
	}
}

.rc-conflict-row {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 10px 16px;
	min-height: 40px;
	padding: 6px 0;
}

.rc-conflict-row > :first-child {
	flex: 1 1 auto;
	min-width: 0;
}

.rc-conflict-row > :last-child {
	flex: none;
	max-width: 60%;
}

.rc-conflict-row + :deep(.rc-toggle-row) {
	border-top: 1px solid var(--a06);
}

.rc-actions {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 10px 12px;
}

.rc-actions__note {
	flex: 1 1 260px;
	min-width: 0;
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	color: var(--t5);
	text-wrap: pretty;
}
</style>
