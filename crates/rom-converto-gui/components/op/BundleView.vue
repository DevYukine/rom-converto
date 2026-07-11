<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { invoke, isTauri, open, save } from "~/lib/ipc";
import { registerDropZone, unregisterDropZone } from "~/composables/useDragDrop";
import { basename, deriveWuaPath } from "~/composables/useDerivedPath";
import { buildCliCommand } from "~/composables/useCliEcho";
import { useQueueStore } from "~/stores/queue";
import { isDiscInput } from "~/stores/wup-compress";
import CliChip from "~/components/ui/CliChip.vue";
import ConfigCard from "~/components/ui/ConfigCard.vue";
import LevelSlider from "~/components/ui/LevelSlider.vue";
import ToggleSwitch from "~/components/ui/ToggleSwitch.vue";
import ConflictPopover from "~/components/modals/ConflictPopover.vue";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";
import DryRunModal from "~/components/modals/DryRunModal.vue";
import type { DryRunLine } from "~/components/modals/DryRunModal.vue";
import type { InfoResult, WupInfo } from "~/types/info";
import { opCommand, opProgressKey } from "~/lib/opdefs/types";
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
		const info = await invoke<InfoResult>("cmd_read_info", {
			input: part.path,
			keys: part.key || null,
		});
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
		void probe(part);
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

async function browseDisc() {
	const picked = await open({ multiple: true, filters: props.def.browseFilters });
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

// Parts still resolving: awaiting a disc key or a failed probe.
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

function humanSize(bytes: number): string {
	if (!bytes) return "0 B";
	const units = ["B", "KiB", "MiB", "GiB", "TiB"];
	let n = bytes;
	let i = 0;
	while (n >= 1024 && i < units.length - 1) {
		n /= 1024;
		i++;
	}
	return `${n.toFixed(i > 1 ? 1 : 0)} ${units[i]}`;
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

function bundleArgs(b: Bundle) {
	return {
		inputs: b.parts.map((p) => p.path),
		output: bundleOutput(b),
		level: store.level,
		keys: b.parts.filter((p) => p.isDisc).map((p) => p.key || ""),
		onConflict: store.onConflict,
		skipSpaceCheck: store.skipSpaceCheck,
	};
}

const partTag: Record<PartKind, string> = { base: "BASE", update: "UPDATE", dlc: "DLC", unknown: "?" };

const cli = computed(() => {
	const b = readyBundles.value[0];
	const args = b
		? bundleArgs(b)
		: {
				inputs: [],
				output: "",
				level: store.level,
				keys: [],
				onConflict: store.onConflict,
				skipSpaceCheck: store.skipSpaceCheck,
			};
	return buildCliCommand("cmd_wup_compress", args);
});

const queued = ref(false);
const addLabel = computed(() =>
	queued.value ? "Bundles queued ✓" : `Add ${readyBundles.value.length} bundles to queue`,
);

function addBundles() {
	if (queued.value || !readyBundles.value.length) return;
	const specs = readyBundles.value.map((b) => {
		const args = bundleArgs(b);
		return {
			name: basename(args.output),
			opLabel: props.def.opLabel,
			command: opCommand(props.def, store),
			args,
			taskId: opProgressKey(props.def, store) ?? "wup-compress",
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
		if (!cmd) cmd = buildCliCommand(opCommand(props.def, store), args);
		let note = "ok";
		let conflict = false;
		try {
			const res = await invoke<{ message?: string }>(opCommand(props.def, store), { ...args, dryRun: true });
			const msg = typeof res === "object" && res ? String(res.message ?? "") : String(res);
			if (msg) note = msg;
			conflict = /exists|rename/i.test(msg);
		} catch (e) {
			note = String(e);
			conflict = true;
		}
		lines.push({ source: bundleName(b), output: args.output, note, conflict });
	}
	dryLines.value = lines;
	dryCommand.value = cmd;
	dryOpen.value = true;
}

function copied() {
	showToast("Copied");
}

const dropEl = ref<HTMLElement | null>(null);
let zoneId: string | null = null;

onMounted(() => {
	if (isTauri && dropEl.value) {
		zoneId = registerDropZone(dropEl.value, (paths) => add(paths), 10);
	}
});

onBeforeUnmount(() => {
	if (zoneId) unregisterDropZone(zoneId);
});
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

		<div ref="dropEl" class="rc-drop">
			<svg class="rc-drop__icon" width="20" height="20" viewBox="0 0 24 24" fill="none"
				stroke="#4593f8" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
				<path d="M4 4h5l2 3h9v11a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2z" />
			</svg>
			<span class="rc-drop__text">{{ def.dropText }}</span>
			<button type="button" class="rc-drop__browse" @click="browseFolder">Browse folder</button>
			<button type="button" class="rc-drop__browse" @click="browseDisc">Browse disc image</button>
		</div>

		<div v-if="unresolved.length" class="rc-card rc-card--warn">
			<div class="rc-card__head">Needs a master key or unreadable</div>
			<div v-for="p in unresolved" :key="p.id" class="rc-part">
				<span class="rc-part__name">{{ p.name }}</span>
				<span class="rc-part__meta">{{ p.error || "reading…" }}</span>
				<button v-if="p.isDisc" type="button" class="rc-link" @click="pickKey(p)">
					{{ p.key ? "Change key…" : "Master key…" }}
				</button>
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
				<span class="rc-bundle__title" :class="{ 'rc-bundle__title--warn': !b.complete }">
					{{ bundleName(b) }}
				</span>
				<span v-if="b.complete" class="rc-bundle__badge">{{ badge(b) }}</span>
				<span v-else class="rc-bundle__note">
					A DLC can't be bundled alone. Drop the base title (00050000{{ b.lowId }}) to complete it
				</span>
				<span class="rc-bundle__spacer" />
				<span v-if="b.complete" class="rc-bundle__total">
					Total {{ humanSize(bundleTotal(b)) }} → {{ basename(bundleOutput(b)) }}
				</span>
				<button v-if="b.complete" type="button" class="rc-link" @click="pickOutput(b)">Change output…</button>
				<button v-else type="button" class="rc-link" @click="browseFolder">Locate base…</button>
			</div>
			<div v-for="p in b.parts" :key="p.id" class="rc-part">
				<span class="rc-tag" :class="`rc-tag--${p.kind}`">{{ partTag[p.kind] }}</span>
				<span class="rc-part__name">{{ p.name }}</span>
				<span class="rc-part__meta">{{ p.titleIdHex }} · v{{ p.version }} · {{ humanSize(p.size) }}</span>
				<button v-if="p.isDisc" type="button" class="rc-link" @click="pickKey(p)">
					{{ p.key ? "key ✓" : "key…" }}
				</button>
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
					:format-value="(v) => (v === 0 ? 'default (0)' : String(v))"
					@update:model-value="store.level = $event"
				/>
			</ConfigCard>

			<ConfigCard title="Safety">
				<div class="rc-conflict-row">
					<span class="rc-conflict-row__label">On conflict</span>
					<ConflictPopover
						:model-value="store.onConflict"
						@update:model-value="store.onConflict = $event"
					/>
				</div>
				<ToggleSwitch
					:model-value="store.skipSpaceCheck"
					label="Skip free-space check"
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
	gap: 14px;
	padding: 18px 26px;
}

.rc-head {
	display: flex;
	align-items: flex-start;
	justify-content: space-between;
	gap: 16px;
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
}

.rc-drop {
	display: flex;
	align-items: center;
	gap: 10px;
	border: 1.5px dashed var(--a22);
	border-radius: 10px;
	padding: 12px 14px;
	color: var(--t4);
	font-size: 12px;
}

.rc-drop:hover,
.rc-drop.drop-hover {
	border-color: #4593f8;
}

.rc-drop__icon {
	flex-shrink: 0;
}

.rc-drop__text {
	flex: 1;
	min-width: 0;
}

.rc-drop__browse {
	border: 1px solid var(--a25);
	border-radius: 6px;
	padding: 4px 12px;
	font-size: 11px;
	color: var(--t0);
	font-weight: 500;
	background: transparent;
	cursor: pointer;
}

.rc-card {
	border: 1px solid var(--a10);
	border-radius: 10px;
	background: var(--card);
	padding: 12px 14px;
	display: flex;
	flex-direction: column;
	gap: 6px;
}

.rc-card--warn {
	border-color: rgba(210, 153, 34, 0.4);
}

.rc-card__head {
	font-size: 10.5px;
	font-weight: 700;
	text-transform: uppercase;
	letter-spacing: 0.8px;
	color: var(--yellow);
}

.rc-bundle__head {
	display: flex;
	align-items: center;
	gap: 10px;
	margin-bottom: 2px;
}

.rc-bundle__title {
	font-weight: 700;
	color: var(--t0);
	font-size: 13px;
}

.rc-bundle__title--warn {
	color: var(--yellow);
}

.rc-bundle__badge {
	font-size: 10.5px;
	color: var(--green);
}

.rc-bundle__note {
	font-size: 10.5px;
	color: var(--t5);
}

.rc-bundle__spacer {
	flex: 1;
}

.rc-bundle__total {
	font-family: ui-monospace, monospace;
	font-size: 10px;
	color: var(--t5);
}

.rc-part {
	display: flex;
	align-items: center;
	gap: 10px;
	padding: 4px 0;
}

.rc-tag {
	flex-shrink: 0;
	width: 52px;
	text-align: center;
	border-radius: 5px;
	padding: 2px 0;
	font-size: 9.5px;
	font-weight: 700;
}

.rc-tag--base {
	background: rgba(69, 147, 248, 0.15);
	color: var(--blue);
}

.rc-tag--update {
	background: rgba(63, 185, 80, 0.15);
	color: var(--green);
}

.rc-tag--dlc {
	background: rgba(210, 153, 34, 0.15);
	color: var(--yellow);
}

.rc-tag--unknown {
	background: var(--a08);
	color: var(--t5);
}

.rc-part__name {
	color: var(--t0);
	font-size: 12px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-part__meta {
	flex: 1;
	font-family: ui-monospace, monospace;
	font-size: 10px;
	color: var(--t5);
}

.rc-link {
	border: none;
	background: transparent;
	color: var(--blue);
	font-size: 11px;
	cursor: pointer;
}

.rc-x {
	border: none;
	background: transparent;
	color: var(--t5);
	cursor: pointer;
	font-size: 12px;
}

.rc-x:hover {
	color: var(--red);
}

.rc-grid {
	display: grid;
	grid-template-columns: repeat(auto-fit, minmax(250px, 1fr));
	gap: 12px;
}

.rc-conflict-row {
	display: flex;
	align-items: center;
	justify-content: space-between;
	padding: 3px 0;
}

.rc-conflict-row__label {
	font-size: 12px;
	color: var(--t2);
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
</style>
