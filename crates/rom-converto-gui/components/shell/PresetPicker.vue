<script setup lang="ts">
import { computed, ref } from "vue";
import { useConfigStore } from "~/stores/config";
import { useChdCompressStore } from "~/stores/chd-compress";
import { useCsoCompressStore } from "~/stores/cso-compress";
import { useDolCompressStore } from "~/stores/dol-compress";
import { useNxCompressStore } from "~/stores/nx-compress";
import { useRvlCompressStore } from "~/stores/rvl-compress";
import { useWupCompressStore } from "~/stores/wup-compress";
import type { OpStore } from "~/lib/opdefs/types";
import type { Preset, PresetFormat } from "~/types/config";

const props = defineProps<{ console: string }>();

const config = useConfigStore();
if (!config.loaded) config.loadConfig();

// Maps a compress console to its preset table plus the config-key -> store-field
// translation. ctr and cue have no config table, so selecting a preset only
// records the active name for them.
const BINDINGS: Record<string, { format: PresetFormat; useStore: () => OpStore; map: Record<string, string> }> = {
	nx: {
		format: "nx",
		useStore: useNxCompressStore,
		map: { level: "level", mode: "mode", block_size_exp: "blockSizeExp", on_conflict: "onConflict", output_dir: "outputDir", report: "reportFile" },
	},
	dol: {
		format: "dol",
		useStore: useDolCompressStore,
		map: { level: "level", chunk_size: "chunkSize", on_conflict: "onConflict", output_dir: "outputDir", report: "reportFile" },
	},
	rvl: {
		format: "rvl",
		useStore: useRvlCompressStore,
		map: { level: "level", chunk_size: "chunkSize", on_conflict: "onConflict", output_dir: "outputDir", report: "reportFile" },
	},
	chd: {
		format: "chd",
		useStore: useChdCompressStore,
		map: {
			hunk_size: "hunkSize",
			codecs: "codecs",
			level: "level",
			on_conflict: "onConflict",
			output_dir: "outputDir",
			report: "reportFile",
		},
	},
	cso: {
		format: "cso",
		useStore: useCsoCompressStore,
		map: { block_size: "blockSize", on_conflict: "onConflict", output_dir: "outputDir", report: "reportFile" },
	},
	wup: {
		format: "wup",
		useStore: useWupCompressStore,
		map: { level: "level", on_conflict: "onConflict" },
	},
};

const open = ref(false);
const current = computed(() => config.activePreset || "None");
const names = computed(() => Object.keys(config.presets).sort());

function isSetValue(value: unknown): boolean {
	if (value === null || value === undefined || value === "") return false;
	return !Array.isArray(value) || value.length > 0;
}

function summary(name: string): string {
	const binding = BINDINGS[props.console];
	const preset = config.presets[name];
	if (!binding || !preset) return "";
	const table = preset[binding.format] as Record<string, unknown> | null | undefined;
	if (!table) return `no ${props.console} settings`;
	const parts: string[] = [];
	for (const key of Object.keys(binding.map)) {
		const value = table[key];
		if (isSetValue(value)) parts.push(`${key}: ${value}`);
	}
	return `${binding.format}: ${parts.join(" · ") || "defaults"}`;
}

function applyToStore(name: string): void {
	const binding = BINDINGS[props.console];
	if (!binding) return;
	const table = config.presets[name]?.[binding.format] as Record<string, unknown> | null | undefined;
	if (!table) return;
	const store = binding.useStore();
	for (const [key, field] of Object.entries(binding.map)) {
		const value = table[key];
		if (value !== null && value !== undefined) store[field] = value;
	}
}

function select(name: string | null) {
	config.applyPreset(name);
	if (name) applyToStore(name);
	open.value = false;
}

const saveName = ref("");
const saving = ref(false);
const saveError = ref("");
// ctr/cue have no preset table; hide the save row where saving is a no-op.
const canSave = computed(() => !!BINDINGS[props.console]);

async function saveCurrent() {
	const trimmed = saveName.value.trim();
	if (!trimmed) return;
	const binding = BINDINGS[props.console];
	if (!binding) return;
	saving.value = true;
	saveError.value = "";
	try {
		const store = binding.useStore();
		const table: Record<string, string | number | string[]> = {};
		for (const [key, field] of Object.entries(binding.map)) {
			const value = store[field];
			if (isSetValue(value)) table[key] = value;
		}
		const preset: Preset = { ...config.presets[trimmed], [binding.format]: table };
		await config.savePreset(trimmed, preset);
		config.applyPreset(trimmed);
		saveName.value = "";
		open.value = false;
	} catch (e) {
		saveError.value = String(e);
	} finally {
		saving.value = false;
	}
}
</script>

<template>
	<div class="preset-wrap">
		<div v-if="open" class="popover">
			<button type="button" class="pop-row" @click="select(null)">
				<span>None</span><span class="pop-dim">(page defaults)</span>
			</button>
			<button v-for="name in names" :key="name" type="button" class="pop-row col" @click="select(name)">
				<span class="pop-strong">{{ name }}</span>
				<span class="pop-sub">{{ summary(name) }}</span>
			</button>
			<div v-if="canSave" class="pop-save">
				<input
					v-model="saveName"
					type="text"
					class="pop-input"
					placeholder="Save current as…"
					@keydown.enter="saveCurrent"
				/>
				<button type="button" class="pop-save-btn" :disabled="!saveName.trim() || saving" @click="saveCurrent">
					{{ saving ? "…" : "Save" }}
				</button>
			</div>
			<p v-if="saveError" class="pop-error">{{ saveError }}</p>
		</div>
		<button type="button" class="preset" aria-haspopup="true" @click="open = !open">
			<span class="p-label">Preset</span>
			<span class="p-value">{{ current }}</span>
			<span class="p-caret">▾</span>
		</button>
	</div>
</template>

<style scoped>
.preset-wrap {
	position: relative;
}
.preset {
	display: flex;
	align-items: center;
	gap: 8px;
	width: 100%;
	border: 1px solid var(--a12);
	border-radius: 8px;
	padding: 8px 10px;
	background: transparent;
	cursor: pointer;
}
.p-label {
	flex: 1;
	color: var(--t3);
	font-size: 12px;
	text-align: left;
}
.p-value {
	color: var(--t0);
	font-weight: 600;
	font-size: 12px;
}
.p-caret {
	color: var(--t4);
}
.popover {
	position: absolute;
	left: 0;
	right: 0;
	bottom: 38px;
	background: var(--pop);
	border: 1px solid var(--a16);
	border-radius: 9px;
	box-shadow: 0 12px 36px var(--shC);
	padding: 4px;
	display: flex;
	flex-direction: column;
	gap: 2px;
}
.pop-row {
	display: flex;
	align-items: center;
	gap: 6px;
	padding: 7px 10px;
	border: none;
	border-radius: 7px;
	background: transparent;
	cursor: pointer;
	text-align: left;
	color: var(--t3);
	font-size: 12px;
}
.pop-row:hover {
	background: var(--a08);
}
.pop-row.col {
	flex-direction: column;
	align-items: flex-start;
	gap: 2px;
}
.pop-dim {
	color: var(--t5);
}
.pop-strong {
	color: var(--t0);
	font-weight: 600;
}
.pop-sub {
	font-family: ui-monospace, monospace;
	font-size: 10.5px;
	color: var(--t4);
}
.pop-save {
	display: flex;
	gap: 6px;
	padding: 6px 4px 2px;
	border-top: 1px solid var(--a08);
	margin-top: 2px;
}
.pop-input {
	flex: 1;
	min-width: 0;
	font-size: 11.5px;
	color: var(--t1);
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: 6px;
	padding: 4px 8px;
}
.pop-save-btn {
	font-size: 11.5px;
	color: var(--t2);
	background: transparent;
	border: 1px solid var(--a14);
	border-radius: 6px;
	padding: 4px 10px;
	cursor: pointer;
}
.pop-save-btn:hover:not(:disabled) {
	border-color: var(--a40);
	color: var(--t0);
}
.pop-save-btn:disabled {
	opacity: 0.5;
	cursor: default;
}
.pop-error {
	font-size: 10.5px;
	color: var(--red);
	padding: 4px;
	margin: 0;
}
</style>
