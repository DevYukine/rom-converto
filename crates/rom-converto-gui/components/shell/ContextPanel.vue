<script setup lang="ts">
import { useUiStore } from "~/stores/ui";
import { opConsoles } from "~/lib/opdefs";
import PresetPicker from "~/components/shell/PresetPicker.vue";

const props = defineProps<{ op: string }>();

const ui = useUiStore();
const route = useRoute();
const router = useRouter();

interface ConsoleRow {
	id: string;
	name: string;
	hint: string;
}

const TITLES: Record<string, string> = {
	compress: "Compress",
	extract: "Extract",
	verify: "Verify",
	decrypt: "Decrypt",
	encrypt: "Encrypt",
	convert: "Convert",
	dat: "DAT",
	tools: "Tools",
};

const SUBTITLES: Record<string, string> = {
	compress: "Console is detected from dropped files, or pick one below.",
	extract: "Decompress back to the raw format.",
	verify: "Integrity checks. No files are written.",
	decrypt: "Remove encryption for emulator use.",
	encrypt: "Re-encrypts decrypted ROMs. Currently only 3DS supports this.",
	convert: "Change container or format.",
	dat: "Match against the Playmatch DAT database.",
	tools: "Utilities that don't convert.",
};

const CONSOLES: Record<string, ConsoleRow[]> = {
	compress: [
		{ id: "ctr", name: "3DS", hint: "→ Z3DS" },
		{ id: "dol", name: "GameCube", hint: "→ RVZ" },
		{ id: "rvl", name: "Wii", hint: "→ RVZ" },
		{ id: "wup", name: "Wii U", hint: "→ WUA" },
		{ id: "nx", name: "Switch", hint: "→ NSZ/XCZ" },
		{ id: "chd", name: "CD / DVD", hint: "→ CHD" },
		{ id: "cso", name: "PSP / PS2", hint: "→ CSO/ZSO" },
	],
	extract: [
		{ id: "ctr", name: "3DS", hint: "Z3DS →" },
		{ id: "dol", name: "GameCube", hint: "RVZ →" },
		{ id: "rvl", name: "Wii", hint: "RVZ →" },
		{ id: "nx", name: "Switch", hint: "NSZ/XCZ →" },
		{ id: "chd", name: "CD / DVD", hint: "CHD →" },
		{ id: "cso", name: "PSP / PS2", hint: "CSO/ZSO →" },
	],
	verify: [
		{ id: "ctr", name: "3DS", hint: "" },
		{ id: "dol", name: "GameCube", hint: "" },
		{ id: "rvl", name: "Wii", hint: "" },
		{ id: "wup", name: "Wii U", hint: "" },
		{ id: "nx", name: "Switch", hint: "NSP/XCI/NSZ" },
		{ id: "chd", name: "CD / DVD (CHD)", hint: "" },
		{ id: "cso", name: "PSP / PS2", hint: "" },
	],
	decrypt: [
		{ id: "ctr", name: "3DS", hint: ".3ds .cci .cia" },
		{ id: "wup", name: "Wii U", hint: "NUS titles" },
	],
	encrypt: [{ id: "ctr", name: "3DS", hint: ".3ds .cci .cia" }],
	convert: [
		{ id: "ctr", name: "3DS", hint: "CIA ↔ CCI" },
		{ id: "cso", name: "PSP / PS2", hint: "ISO → CHD" },
		{ id: "chd", name: "CD / DVD", hint: "CHD → CSO/ZSO" },
		{ id: "cue", name: "CD (CUE/BIN)", hint: "→ ISO/CSO/ZSO" },
	],
	dat: [
		{ id: "scan", name: "Scan", hint: "" },
		{ id: "verify", name: "Verify", hint: "" },
		{ id: "rename", name: "Rename", hint: "" },
	],
	tools: [
		{ id: "playlist", name: "Playlist (.m3u)", hint: "" },
		{ id: "hash", name: "Hash", hint: "" },
		{ id: "merge", name: "Merge multi-bin", hint: "" },
		{ id: "cdn2cia", name: "CDN → CIA", hint: "" },
		{ id: "ticket", name: "Generate ticket", hint: "" },
	],
};

// Registered consoles missing from the curated list still get a row so nothing
// in the registry is unreachable from the picker.
const rows = computed<ConsoleRow[]>(() => {
	const listed = CONSOLES[props.op] ?? [];
	const known = new Set(listed.map((r) => r.id));
	const extra = opConsoles(props.op)
		.filter((id) => !known.has(id))
		.map((id) => ({ id, name: id, hint: "" }));
	return [...listed, ...extra];
});
const activeConsole = computed(() => route.path.split("/")[2] ?? "");

function pick(id: string) {
	ui.setLastConsole(props.op, id);
	router.push(`/${props.op}/${id}`);
}
</script>

<template>
	<aside class="panel">
		<div class="title">{{ TITLES[op] }}</div>
		<p class="subtitle">{{ SUBTITLES[op] }}</p>

		<div class="rows">
			<button
				v-for="row in rows"
				:key="row.id"
				type="button"
				class="row"
				:class="{ active: activeConsole === row.id }"
				@click="pick(row.id)"
			>
				<span class="name">{{ row.name }}</span>
				<span class="hint">{{ row.hint }}</span>
			</button>
		</div>

		<p v-if="op === 'encrypt'" class="note">
			Other consoles don't appear here because they have no encrypt operation.
		</p>

		<div class="spacer" />

		<PresetPicker v-if="op === 'compress'" :console="activeConsole" />
	</aside>
</template>

<style scoped>
.panel {
	display: flex;
	flex-direction: column;
	width: 230px;
	flex-shrink: 0;
	min-height: 0;
	overflow-y: auto;
	overflow-x: hidden;
	background: var(--bg3);
	border-right: 1px solid var(--a10);
	padding: 16px 10px 12px;
}
.title {
	font-size: 15px;
	font-weight: 700;
	color: var(--t0);
}
.subtitle {
	margin: 6px 0 12px;
	font-size: 11px;
	color: var(--t4);
	line-height: 1.45;
}
.rows {
	display: flex;
	flex-direction: column;
	gap: 2px;
}
.row {
	display: flex;
	align-items: center;
	gap: 8px;
	padding: 7px 10px;
	border: none;
	border-radius: 8px;
	background: transparent;
	color: var(--t3);
	font-size: 12px;
	font-weight: 400;
	cursor: pointer;
	text-align: left;
}
.row:hover {
	background: var(--a08);
}
.row.active {
	color: var(--t0);
	background: var(--a12);
	font-weight: 600;
}
.name {
	flex: 1;
}
.hint {
	font-family: ui-monospace, monospace;
	font-size: 10px;
	color: var(--t5);
}
.row.active .hint {
	color: var(--blue);
}
.note {
	margin-top: 8px;
	font-size: 10.5px;
	color: var(--t6);
	line-height: 1.4;
}
.spacer {
	flex: 1;
}
</style>
