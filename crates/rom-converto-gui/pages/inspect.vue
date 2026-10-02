<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { invoke, open } from "~/lib/ipc";
import { opDef } from "~/lib/opdefs";
import { useQueueStore } from "~/stores/queue";
import { useToast } from "~/composables/useToast";
import { basename } from "~/composables/useDerivedPath";
import DropZone from "~/components/op/DropZone.vue";
import InspectCard from "~/components/op/InspectCard.vue";
import type { InfoResult } from "~/types";
import { opCommand, opProgressKey } from "~/lib/opdefs/types";
import type { StagedItem } from "~/lib/opdefs/types";

const queue = useQueueStore();
const { show: showToast } = useToast();

const KEYS_STORAGE_KEY = "rom-converto:inspect-keys";

function readPersistedKeys(): string {
	try {
		return localStorage.getItem(KEYS_STORAGE_KEY) || "";
	} catch {
		// localStorage unavailable; fall back to no default.
		return "";
	}
}

const input = ref("");
const keysPath = ref(readPersistedKeys());
const rawJson = ref("");
const info = ref<InfoResult | null>(null);
const loading = ref(false);
const error = ref("");

watch(keysPath, (v) => {
	try {
		if (v) localStorage.setItem(KEYS_STORAGE_KEY, v);
		else localStorage.removeItem(KEYS_STORAGE_KEY);
	} catch {
		// localStorage unavailable; keysPath just won't persist.
	}
});

// Guards against overlapping loads: a slow response for the previous file
// must not overwrite the info of the one picked after it.
let loadSeq = 0;

async function load() {
	if (!input.value) return;
	const seq = ++loadSeq;
	loading.value = true;
	error.value = "";
	info.value = null;
	rawJson.value = "";
	try {
		const json = await invoke<string>("cmd_read_info", { input: input.value, keys: keysPath.value || null });
		if (seq !== loadSeq) return;
		rawJson.value = json;
		info.value = JSON.parse(json) as InfoResult;
	} catch (e) {
		if (seq === loadSeq) error.value = String(e);
	} finally {
		if (seq === loadSeq) loading.value = false;
	}
}

function onAdd(paths: string[]) {
	const path = paths[0];
	if (!path) return;
	input.value = path;
	void load();
}

async function browseKeys() {
	const picked = await open({ multiple: false });
	if (typeof picked === "string") {
		keysPath.value = picked;
		if (input.value) void load();
	}
}

function sizeOf(i: InfoResult): number {
	if (i.kind === "wup") return i.total_content_size;
	if (i.kind === "xbox") return i.image_size;
	if (i.kind === "xenon") return i.compressed_size;
	if (i.kind === "ps3" || i.kind === "psx" || i.kind === "psp") return i.size_bytes;
	if (i.kind === "laser_disc") return i.file_size_bytes;
	if (i.kind === "retro") return i.file_size;
	if (i.kind === "vpk" || i.kind === "pkg") return i.total_size;
	if (i.kind === "ps4_pkg" || i.kind === "ps5_pkg") return i.file_size;
	return i.physical_bytes;
}

const compressDef = computed(() => (info.value ? opDef("compress", info.value.kind) : undefined));
const verifyDef = computed(() => (info.value ? opDef("verify", info.value.kind) : undefined));

function runQuick(kind: "compress" | "verify") {
	const def = kind === "compress" ? compressDef.value : verifyDef.value;
	if (!def || !info.value) return;
	const store = def.useStore();
	const taskId = `job-${crypto.randomUUID()}`;
	const item: StagedItem = {
		id: crypto.randomUUID(),
		path: input.value,
		name: basename(input.value),
		size: sizeOf(info.value),
		outExt: "",
	};
	queue.enqueue([
		{
			name: item.name,
			opLabel: def.opLabel,
			command: opCommand(def, store),
			args: def.buildArgs(store, item, taskId),
			taskId,
			progressKey: opProgressKey(def, store),
			chips: def.chips(store),
			resultKind: def.resultKind,
			routeBack: { storeId: def.storeId },
			inputBytes: item.size,
		},
	]);
	queue.drawerOpen = true;
	showToast(kind === "compress" ? "Compression queued" : "Verification queued");
}
</script>

<template>
	<div class="rc-inspect rc-page">
		<div class="rc-inspect__header">
			<h1>Inspect ROM</h1>
			<p>Reads metadata instantly. Nothing enters the queue and nothing is written.</p>
		</div>

		<DropZone
			drop-text="Drop any supported ROM, disc image, container, archive or title folder"
			also-directory
			@add="onAdd"
		/>

		<div class="rc-inspect__keys">
			<FieldLabel
				label="Keys (optional)"
				tooltip="prod.keys for Switch containers, an optional master key override for Wii U discs. Other consoles do not need it."
			/>
			<button type="button" class="rc-picker" :title="keysPath || 'Browse keys'" aria-label="Browse keys" @click="browseKeys">
					<span v-if="keysPath" class="rc-picker__value rc-inspect__keys-path">{{ keysPath }}</span>
				<span v-else class="rc-inspect__keys-placeholder">Not set</span>
				<svg class="rc-picker__icon" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" aria-hidden="true">
					<path d="M3 7h6l2 2h10v10H3z" />
				</svg>
			</button>
		</div>

		<p v-if="loading" class="rc-inspect__status">Reading metadata…</p>
		<p v-else-if="error" class="rc-inspect__status rc-inspect__status--error">{{ error }}</p>
		<p v-else-if="!info" class="rc-inspect__status">No file inspected yet.</p>

		<InspectCard
			v-if="info"
			:info="info"
			:raw-json="rawJson"
			:path="input"
			:can-compress="!!compressDef"
			:can-verify="!!verifyDef"
			@compress="runQuick('compress')"
			@verify="runQuick('verify')"
		/>
	</div>
</template>

<style scoped>
.rc-inspect {
	width: 100%;
	max-width: 900px;
	margin: 0 auto;
	padding: 24px 28px 32px;
	display: flex;
	flex-direction: column;
	gap: 16px;
}

.rc-inspect__header h1 {
	font-size: var(--fs-xl);
	font-weight: 700;
	color: var(--t0);
	line-height: 1.25;
	text-wrap: balance;
}

.rc-inspect__header p {
	margin-top: 4px;
	font-size: var(--fs-md);
	line-height: var(--lh-body);
	color: var(--t4);
	text-wrap: pretty;
}

.rc-inspect__keys {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 10px 16px;
	min-height: 32px;
	padding: 6px 0;
}

.rc-inspect__keys > :deep(.rc-field-label) {
	flex: 1 1 auto;
	min-width: 0;
}

.rc-inspect__keys .rc-picker {
	flex: 0 1 60%;
	min-width: 160px;
	margin-left: auto;
}

.rc-inspect__keys-path {
	font-family: var(--font-mono);
	color: var(--t3);
}

.rc-inspect__keys-placeholder {
	color: var(--t6);
}

.rc-inspect__status {
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	color: var(--t4);
	text-wrap: pretty;
	overflow-wrap: anywhere;
}

.rc-inspect__status--error {
	color: var(--red);
}
</style>
