<script setup lang="ts">
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { invoke, isTauri } from "~/lib/ipc";
import { useConfigStore } from "~/stores/config";
import { useUpdatesStore } from "~/stores/updates";
import { useUiStore } from "~/stores/ui";
import { useJobConcurrency } from "~/composables/useJobConcurrency";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";
import type { Preset, PresetFormat } from "~/types";

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
	{ label: "150%", value: "1.5" },
	{ label: "200%", value: "2" },
];

function setScale(raw: string | number) {
	ui.scale = Number(raw) as typeof ui.scale;
}


const FORMAT_LABELS: Record<PresetFormat, string> = {
	dol: "GameCube (dol)",
	rvl: "Wii (rvl)",
	nx: "Switch (nx)",
	chd: "CHD",
	cso: "CSO/ZSO",
	wup: "Wii U (wup)",
	dat: "DAT",
	organize: "Organize",
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
const updates = useUpdatesStore();
const updateState = toRef(updates, "state");
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
	if (updateState.value.phase === "available") return updates.install();
	return updates.check();
}

async function openChangelog() {
	const url = "https://github.com/DevYukine/rom-converto/releases/latest";
	if (isTauri) await openExternal(url);
	else window.open(url, "_blank", "noopener,noreferrer");
}

onMounted(async () => {
	version.value = await invoke<string>("app_display_version");
});
</script>

<template>
	<div class="page rc-page">
		<h1>Settings</h1>

		<div class="cards">
			<ConfigCard title="Appearance">
				<div class="row">
					<div class="row__text">
						<FieldLabel label="Theme" tooltip="Light and dark stay fixed regardless of the OS. Follow OS is the only option that changes with it." />
						<p class="caption">Follow OS switches automatically with your system.</p>
					</div>
					<Segmented
						aria-label="Theme"
						:model-value="ui.theme"
						:options="THEME_OPTIONS"
						@update:model-value="(v) => (ui.theme = v as typeof ui.theme)"
					/>
				</div>
				<div class="row">
					<div class="row__text">
						<FieldLabel label="Interface scale" tooltip="Scales the size of the whole interface, not just text." />
						<p class="caption">Tunes density for 2K+ or small displays. The layout is fluid either way.</p>
					</div>
					<Segmented
						aria-label="Interface scale"
						:model-value="String(ui.scale)"
						:options="SCALE_OPTIONS"
						@update:model-value="setScale"
					/>
				</div>
			</ConfigCard>

			<ConfigCard title="Global queue">
				<div class="row">
					<div class="row__text">
						<FieldLabel
							label="Concurrent jobs"
							tooltip="More jobs finish the queue faster but compete for CPU and disk."
						/>
						<p class="caption">How many jobs run at once (1 to 8). Separate from per-format worker threads.</p>
					</div>
					<Stepper v-model="concurrency" :min="1" :max="maxConcurrency" label="Concurrent jobs" />
				</div>
				<ToggleSwitch
					v-model="ui.startImmediately"
					label="Start jobs immediately"
					tooltip="When on, added jobs start right away instead of waiting for you to press Start."
					description="When off, jobs wait until you press Start in the queue."
				/>
				<ToggleSwitch
					v-model="ui.taskbarProgress"
					label="Taskbar / dock progress"
					tooltip="Works on the Windows taskbar and the macOS or Linux dock icon."
					description="Mirror queue progress on the app icon. Turns red on failure."
				/>
				<ToggleSwitch
					v-model="ui.soundEnabled"
					label="Completion sound"
					tooltip="Plays a sound when the queue finishes."
				/>
				<div class="row">
					<div class="row__text">
						<FieldLabel
							label="Default on-conflict policy"
							tooltip="Applied to new jobs unless a page overrides it. What to do when the output file already exists. Organize always starts at Error regardless of this setting."
						/>
						<p class="caption">Pages can still override before queuing. Organize always starts at Error.</p>
					</div>
					<ConflictPopover v-model="ui.defaultOnConflict" />
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
							<button type="button" class="preset-name" :title="name" @click="store.applyPreset(name)">
								{{ name }}
							</button>
							<span v-if="store.activePreset === name" class="pill">active</span>
							<span class="preset-summary" :title="summary(store.presets[name])">{{ summary(store.presets[name]) }}</span>
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
				<ToggleSwitch
					v-model="updates.autoCheck"
					label="Check for updates automatically"
					tooltip="Checks shortly after launch and every four hours while the app is open. Nothing installs until you choose to."
					description="Shows a small notice when a new version is available."
				/>
				<div class="row">
					<span class="status" role="status" aria-live="polite">{{ updateStatus }}</span>
					<span class="row__buttons">
						<PrimaryButton variant="outlined" :disabled="updateBusy || (updateState.phase === 'available' && updates.blocked)" @click="updateAction">
							{{ updateState.phase === "available" ? "Install update" : "Check now" }}
						</PrimaryButton>
						<PrimaryButton variant="outlined" @click="openChangelog">Changelog</PrimaryButton>
					</span>
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
	padding: 24px 28px 32px;
	max-width: 860px;
	margin-inline: auto;
}

h1 {
	font-size: var(--fs-xl);
	font-weight: 700;
	color: var(--t0);
	line-height: 1.25;
	text-wrap: balance;
	margin-bottom: 16px;
}

.cards {
	display: flex;
	flex-direction: column;
	gap: 16px;
}

.row {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 10px 16px;
	min-height: 40px;
	padding: 6px 0;
}

:deep(.row + .row),
:deep(.row + .rc-toggle-row),
:deep(.rc-toggle-row + .rc-toggle-row),
:deep(.rc-kv + .rc-kv),
:deep(.row + .rc-kv),
:deep(.rc-kv + .row) {
	border-top: 1px solid var(--a06);
}

.row__text {
	flex: 1 1 240px;
	min-width: 0;
}

.row > :deep(.rc-segmented-wrap),
.row > :deep(.rc-conflict) {
	flex: none;
	max-width: 100%;
}

.row__buttons {
	display: flex;
	flex-wrap: wrap;
	gap: 8px;
}

.row__label {
	flex: 1 1 240px;
	min-width: 0;
	font-size: var(--fs-md);
	color: var(--t2);
	overflow-wrap: anywhere;
}

.caption {
	font-size: var(--fs-sm);
	color: var(--t5);
	line-height: var(--lh-body);
	text-wrap: pretty;
	margin-top: 4px;
}


.path {
	font-family: var(--font-mono);
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	color: var(--t4);
	overflow-wrap: anywhere;
	margin: 0 0 8px;
}

.presets {
	list-style: none;
	margin: 0;
	padding: 0;
}

.preset-row {
	display: flex;
	flex-wrap: wrap;
	align-items: center;
	gap: 8px 16px;
	padding: 6px 0;
	min-height: 32px;
	border-top: 1px solid var(--a06);
}

.preset-row:first-child {
	border-top: none;
}

.preset-row__main {
	display: flex;
	flex: 1 1 240px;
	align-items: center;
	gap: 8px;
	min-width: 0;
}

.preset-name {
	min-width: 0;
	background: none;
	border: none;
	padding: 0;
	font-size: var(--fs-md);
	font-weight: 600;
	color: var(--t0);
	cursor: pointer;
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}

.pill {
	flex: none;
	font-size: var(--fs-xs);
	font-weight: 600;
	color: var(--blue);
	background: var(--tint-blue);
	border-radius: var(--r-sm);
	padding: 2px 7px;
	white-space: nowrap;
}

.preset-summary {
	min-width: 0;
	font-size: var(--fs-xs);
	color: var(--t4);
	background: var(--a06);
	border-radius: var(--r-sm);
	padding: 2px 7px;
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}

.preset-row__actions {
	display: flex;
	flex: none;
	align-items: center;
	gap: 4px;
}

.link {
	flex: none;
	background: none;
	border: none;
	border-radius: var(--r-sm);
	min-height: var(--ctl-h);
	padding: 0 8px;
	font-size: var(--fs-sm);
	color: var(--t4);
	cursor: pointer;
	white-space: nowrap;
}

.link:hover {
	background: var(--a06);
	color: var(--t0);
}

.link.danger {
	color: var(--red);
}

.dat {
	margin-top: 8px;
	padding-top: 8px;
	border-top: 1px solid var(--a06);
}

.note {
	margin: 10px 0 0;
	font-size: var(--fs-sm);
	color: var(--t5);
	line-height: var(--lh-body);
	text-wrap: pretty;
}

.note code {
	font-family: var(--font-mono);
}

.rc-error {
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	color: var(--red);
	overflow-wrap: anywhere;
}

.status {
	flex: 1 1 240px;
	min-width: 0;
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	color: var(--t2);
	text-wrap: pretty;
}
</style>
