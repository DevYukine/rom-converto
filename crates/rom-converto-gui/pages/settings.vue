<script setup lang="ts">
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { invoke, isTauri } from "~/lib/ipc";
import { createUpdater, type UpdateState } from "~/lib/updater";
import { useConfigStore } from "~/stores/config";
import { useUiStore } from "~/stores/ui";
import { useJobConcurrency } from "~/composables/useJobConcurrency";
import type { Preset, PresetFormat } from "~/types/config";

const store = useConfigStore();
if (!store.loaded) store.loadConfig();

const ui = useUiStore();
const { concurrency, maxConcurrency } = useJobConcurrency();

const THEME_OPTIONS = [
	{ label: "Follow OS", value: "system" },
	{ label: "Light", value: "light" },
	{ label: "Dark", value: "dark" },
];

const SCALE_OPTIONS = [
	{ label: "90%", value: "0.9" },
	{ label: "100%", value: "1" },
	{ label: "115%", value: "1.15" },
	{ label: "130%", value: "1.3" },
];

function setScale(raw: string) {
	ui.scale = Number(raw) as 0.9 | 1.0 | 1.15 | 1.3;
}

function stepConcurrency(delta: number) {
	concurrency.value = Math.min(maxConcurrency, Math.max(1, concurrency.value + delta));
}

const FORMAT_LABELS: Record<PresetFormat, string> = {
	dol: "GameCube (dol)",
	rvl: "Wii (rvl)",
	nx: "Switch (nx)",
	chd: "CHD",
	cso: "CSO/ZSO",
	wup: "Wii U (wup)",
	dat: "DAT",
};

const presetNames = computed(() => Object.keys(store.presets).sort());

function summary(preset: Preset | undefined): string {
	if (!preset) return "empty";
	const formats = (Object.keys(preset) as PresetFormat[]).filter((k) => preset[k]);
	return formats.map((f) => FORMAT_LABELS[f] ?? f).join(" · ") || "empty";
}

const editingPreset = ref<string | null>(null);

// Two-click confirm instead of a modal: the first click arms the button,
// which disarms itself after a moment if the user hesitates.
const confirmingDelete = ref<string | null>(null);
let confirmTimer: ReturnType<typeof setTimeout> | undefined;

async function deletePreset(name: string) {
	if (confirmingDelete.value !== name) {
		confirmingDelete.value = name;
		clearTimeout(confirmTimer);
		confirmTimer = setTimeout(() => (confirmingDelete.value = null), 3000);
		return;
	}
	clearTimeout(confirmTimer);
	confirmingDelete.value = null;
	try {
		await store.deletePreset(name);
	} catch {
		// surfaced via store.error below
	}
}

const version = ref("");
const updateState = ref<UpdateState>({ phase: "current", availableVersion: "", error: "" });
const updater = createUpdater(isTauri, (state) => (updateState.value = state));
const updateBusy = computed(() => ["checking", "downloading", "installing"].includes(updateState.value.phase));
const updateStatus = computed(() => {
	const current = `v${version.value || "…"}`;
	switch (updateState.value.phase) {
		case "checking": return `${current} · checking for updates`;
		case "available": return `${current} · v${updateState.value.availableVersion} available`;
		case "downloading": return `${current} · downloading v${updateState.value.availableVersion}`;
		case "installing": return `${current} · installing v${updateState.value.availableVersion}`;
		case "up-to-date": return `${current} · up to date`;
		case "error": return `${current} · ${updateState.value.error}`;
		default: return `${current} · update not checked`;
	}
});

function updateAction() {
	if (updateState.value.phase === "available") return updater.installUpdate();
	if (updateState.value.phase === "error") return updater.checkForUpdate();
	return openChangelog();
}

async function openChangelog() {
	const url = "https://github.com/DevYukine/rom-converto/releases/latest";
	if (isTauri) await openExternal(url);
	else window.open(url, "_blank", "noopener,noreferrer");
}

onMounted(async () => {
	if (isTauri) void updater.checkForUpdate();
	version.value = await invoke<string>("app_display_version");
});
</script>

<template>
	<div class="page">
		<h1>Settings</h1>

		<div class="cards">
			<ConfigCard title="Appearance">
				<div class="row">
					<Segmented
						label="Theme"
						:model-value="ui.theme"
						:options="THEME_OPTIONS"
						@update:model-value="(v: string) => (ui.theme = v as typeof ui.theme)"
					/>
					<p class="caption">Follow OS switches automatically with your system.</p>
				</div>
				<div class="row">
					<Segmented
						label="Interface scale"
						:model-value="String(ui.scale)"
						:options="SCALE_OPTIONS"
						@update:model-value="setScale"
					/>
					<p class="caption">Tunes density for 2K+ or small displays. The layout is fluid either way.</p>
				</div>
			</ConfigCard>

			<ConfigCard title="Global queue">
				<div class="row">
					<span class="row__label">Concurrent jobs</span>
					<span class="stepper">
						<button type="button" aria-label="Fewer concurrent jobs" @click="stepConcurrency(-1)">−</button>
						{{ concurrency }}
						<button type="button" aria-label="More concurrent jobs" @click="stepConcurrency(1)">+</button>
					</span>
					<p class="caption">How many jobs run at once (1 to 8). Separate from per-format worker threads.</p>
				</div>
				<ToggleSwitch
					v-model="ui.startImmediately"
					label="Start jobs immediately"
					description="When off, jobs wait until you press Start in the queue."
				/>
				<ToggleSwitch
					v-model="ui.taskbarProgress"
					label="Taskbar / dock progress"
					description="Mirror queue progress on the app icon. Turns red on failure."
				/>
				<ToggleSwitch
					v-model="ui.soundEnabled"
					label="Completion sound"
				/>
				<div class="row">
					<span class="row__label">Default on-conflict policy</span>
					<ConflictPopover v-model="ui.defaultOnConflict" />
					<p class="caption">Pages can still override before queuing.</p>
				</div>
			</ConfigCard>

			<ConfigCard title="Presets">
				<p class="path">{{ store.configPath ?? "no config file found yet; saving a preset creates one" }}</p>
				<p v-if="store.error" class="rc-error">{{ store.error }}</p>

				<div v-if="store.activePreset" class="row">
					<span class="row__label">Active preset: <strong>{{ store.activePreset }}</strong></span>
					<button type="button" class="link" @click="store.applyPreset(null)">Clear active preset</button>
				</div>

				<p v-if="presetNames.length === 0" class="caption">No presets yet.</p>
				<ul v-else class="presets">
					<li v-for="name in presetNames" :key="name" class="preset-row">
						<div class="preset-row__main">
							<button type="button" class="preset-name" @click="store.applyPreset(name)">
								{{ name }}
							</button>
							<span v-if="store.activePreset === name" class="pill">active</span>
							<span class="preset-summary">{{ summary(store.presets[name]) }}</span>
						</div>
						<div class="preset-row__actions">
							<button type="button" class="link" @click="editingPreset = name">Edit</button>
							<button
								type="button"
								class="link"
								:class="{ danger: confirmingDelete === name }"
								@click="deletePreset(name)"
							>
								{{ confirmingDelete === name ? "Confirm delete" : "Delete" }}
							</button>
						</div>
					</li>
				</ul>

				<div v-if="store.dat" class="dat">
					<KvRow label="Checksum floor" :value="store.dat.input_checksum_min ?? 'crc32 (default)'" />
					<KvRow label="Checksum ceiling" :value="store.dat.input_checksum_max ?? 'sha256 (default)'" />
				</div>

				<p class="note">A preset saved here runs identically from the CLI with <code>--preset &lt;name&gt;</code>.</p>
			</ConfigCard>

			<ConfigCard title="Updates">
				<div class="row">
					<span class="status" role="status" aria-live="polite">{{ updateStatus }}</span>
					<button type="button" class="outlined" :disabled="updateBusy" @click="updateAction">
						{{ updateState.phase === "available" ? "Install update" : updateState.phase === "error" ? "Retry" : "Changelog" }}
					</button>
				</div>
			</ConfigCard>
		</div>

		<PresetEditModal
			v-if="editingPreset"
			:name="editingPreset"
			:preset="store.presets[editingPreset]!"
			@close="editingPreset = null"
		/>
	</div>
</template>

<style scoped>
.page {
	padding: 22px 28px;
	max-width: 860px;
	margin-inline: auto;
}

h1 {
	font-size: 18px;
	font-weight: 700;
	color: var(--t0);
	margin-bottom: 14px;
}

.cards {
	display: flex;
	flex-direction: column;
	gap: 14px;
}

.row {
	display: flex;
	flex-direction: column;
	align-items: flex-start;
	gap: 4px;
	padding: 6px 0;
}

.row__label {
	font-size: 12px;
	color: var(--t2);
}

.caption {
	font-size: 10.5px;
	color: var(--t5);
	line-height: 1.45;
}

.stepper {
	display: flex;
	align-items: center;
	gap: 8px;
	color: var(--t2);
	font-size: 12px;
}

.stepper button {
	background: transparent;
	border: 1px solid var(--a14);
	border-radius: 6px;
	color: var(--t3);
	cursor: pointer;
	width: 22px;
	height: 22px;
	line-height: 1;
}

.stepper button:hover {
	border-color: var(--a40);
	color: var(--t0);
}

.path {
	font-family: ui-monospace, monospace;
	font-size: 11px;
	color: var(--t4);
	word-break: break-all;
	margin: 0 0 8px;
}

.presets {
	list-style: none;
	margin: 0;
	padding: 0;
	display: flex;
	flex-direction: column;
	gap: 2px;
}

.preset-row {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 10px;
	padding: 7px 0;
	border-top: 1px solid var(--a08);
}

.preset-row:first-child {
	border-top: none;
}

.preset-row__main {
	display: flex;
	align-items: center;
	gap: 8px;
	min-width: 0;
}

.preset-name {
	background: none;
	border: none;
	padding: 0;
	font-size: 12.5px;
	font-weight: 700;
	color: var(--t0);
	cursor: pointer;
}

.pill {
	font-size: 10px;
	font-weight: 600;
	color: var(--blue);
	background: rgba(69, 147, 248, .14);
	border-radius: 5px;
	padding: 1px 7px;
}

.preset-summary {
	font-size: 11px;
	color: var(--t4);
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}

.preset-row__actions {
	display: flex;
	align-items: center;
	gap: 12px;
	flex-shrink: 0;
}

.link {
	background: none;
	border: none;
	padding: 0;
	font-size: 11.5px;
	color: var(--t4);
	cursor: pointer;
}

.link:hover {
	color: var(--t0);
	text-decoration: underline;
}

.link.danger {
	color: var(--red);
}

.dat {
	margin-top: 8px;
	padding-top: 8px;
	border-top: 1px solid var(--a08);
}

.note {
	margin: 10px 0 0;
	font-size: 10.5px;
	color: var(--t5);
	line-height: 1.45;
}

.rc-error {
	font-size: 11.5px;
	color: var(--red);
}

.status {
	font-size: 12px;
	color: var(--t2);
}

.outlined {
	background: none;
	border: 1px solid var(--a18);
	color: var(--t3);
	border-radius: 8px;
	padding: 5px 14px;
	font-size: 12px;
	cursor: pointer;
}

.outlined:hover {
	border-color: var(--a40);
}
</style>
