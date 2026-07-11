<script setup lang="ts">
import { computed, ref } from "vue";
import { invoke, open } from "~/lib/ipc";
import { opDef } from "~/lib/opdefs";
import { useQueueStore } from "~/stores/queue";
import { useToast } from "~/composables/useToast";
import { basename } from "~/composables/useDerivedPath";
import DropZone from "~/components/op/DropZone.vue";
import InspectCard from "~/components/op/InspectCard.vue";
import type { InfoResult } from "~/types/info";
import { opCommand, opProgressKey } from "~/lib/opdefs/types";
import type { StagedItem } from "~/lib/opdefs/types";

const queue = useQueueStore();
const { show: showToast } = useToast();

const input = ref("");
const keysPath = ref("");
const rawJson = ref("");
const info = ref<InfoResult | null>(null);
const loading = ref(false);
const error = ref("");

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
	return i.kind === "wup" ? i.total_content_size : i.physical_bytes;
}

const compressDef = computed(() => (info.value ? opDef("compress", info.value.kind) : undefined));
const verifyDef = computed(() => (info.value ? opDef("verify", info.value.kind) : undefined));

function runQuick(kind: "compress" | "verify") {
	const def = kind === "compress" ? compressDef.value : verifyDef.value;
	if (!def || !info.value) return;
	const store = def.useStore();
	const taskId = opProgressKey(def, store) ?? `job-${crypto.randomUUID()}`;
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
	<div class="rc-inspect">
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
			<span>Keys (optional)</span>
			<span class="rc-inspect__keys-path">{{ keysPath || "not set" }}</span>
			<button type="button" @click="browseKeys">Browse</button>
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
	max-width: 900px;
	margin: 0 auto;
	padding: 20px 26px;
	display: flex;
	flex-direction: column;
	gap: 12px;
}

.rc-inspect__header h1 {
	font-size: 18px;
	font-weight: 700;
	color: var(--t0);
}

.rc-inspect__header p {
	margin-top: 4px;
	font-size: 11.5px;
	color: var(--t4);
}

.rc-inspect__keys {
	display: flex;
	align-items: center;
	gap: 8px;
	font-size: 11.5px;
	color: var(--t4);
}

.rc-inspect__keys-path {
	flex: 1;
	min-width: 0;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
	font-family: ui-monospace, monospace;
	color: var(--t3);
}

.rc-inspect__keys button {
	border: 1px solid var(--a25);
	border-radius: 6px;
	padding: 4px 12px;
	font-size: 11px;
	color: var(--t0);
	font-weight: 500;
	background: transparent;
	cursor: pointer;
}

.rc-inspect__status {
	font-size: 12px;
	color: var(--t4);
}

.rc-inspect__status--error {
	color: #d43a3e;
}
</style>
