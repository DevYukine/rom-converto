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
	organize: "Organize",
	dat: "DAT",
	tools: "Tools",
};

const SUBTITLES: Record<string, string> = {
	compress: "Console is detected from dropped files, or pick one below.",
	extract: "Decompress back to the raw format.",
	verify: "Integrity checks. No files are written.",
	decrypt: "Remove encryption for emulator use.",
	encrypt: "Re-encrypts decrypted ROMs.",
	convert: "Change container or format.",
	organize: "Sort a library into per-console folders in one pass.",
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
		{ id: "xenon", name: "Xbox 360", hint: "→ ZAR" },
	],
	extract: [
		{ id: "ctr", name: "3DS", hint: "Z3DS →" },
		{ id: "dol", name: "GameCube", hint: "RVZ →" },
		{ id: "rvl", name: "Wii", hint: "RVZ →" },
		{ id: "nx", name: "Switch", hint: "NSZ/XCZ →" },
		{ id: "chd", name: "CD / DVD", hint: "CHD →" },
		{ id: "cso", name: "PSP / PS2", hint: "CSO/ZSO →" },
		{ id: "xbox", name: "Xbox", hint: "XISO →" },
		{ id: "xenon", name: "Xbox 360", hint: "ZAR →" },
		{ id: "psp", name: "PSP", hint: "PBP →" },
		{ id: "vita", name: "PS Vita", hint: "PKG →" },
	],
	verify: [
		{ id: "ctr", name: "3DS", hint: "" },
		{ id: "dol", name: "GameCube", hint: "" },
		{ id: "rvl", name: "Wii", hint: "" },
		{ id: "wup", name: "Wii U", hint: "" },
		{ id: "nx", name: "Switch", hint: "" },
		{ id: "chd", name: "CD / DVD (CHD)", hint: "" },
		{ id: "cso", name: "PSP / PS2", hint: "" },
		{ id: "xenon", name: "Xbox 360", hint: "" },
	],
	decrypt: [
		{ id: "ctr", name: "3DS", hint: ".3ds .cci .cia" },
		{ id: "wup", name: "Wii U", hint: "NUS titles" },
		{ id: "ps3", name: "PlayStation 3", hint: "built-in keys" },
		{ id: "ntr", name: "Nintendo DS", hint: "KEY1 secure area" },
		{ id: "nx", name: "Switch", hint: "needs prod.keys" },
	],
	encrypt: [
		{ id: "ctr", name: "3DS", hint: ".3ds .cci .cia" },
		{ id: "ntr", name: "Nintendo DS", hint: "KEY1 secure area" },
	],
	convert: [
		{ id: "ctr", name: "3DS", hint: "CIA ↔ CCI" },
		{ id: "wup", name: "Wii U", hint: "WUD ↔ WUX" },
		{ id: "cso", name: "PSP / PS2", hint: "CSO → CHD" },
		{ id: "chd", name: "CD / DVD", hint: "CHD → CSO/ZSO" },
		{ id: "chd-migrate", name: "CHD (old)", hint: "v1-v4 → v5" },
		{ id: "cue", name: "CD (CUE/BIN)", hint: "→ ISO/CSO/ZSO" },
		{ id: "xbox", name: "Xbox", hint: "ISO → XISO" },
		{ id: "psp", name: "PSP", hint: "PBP → ISO" },
		{ id: "xenon", name: "Xbox 360", hint: "ISO → GoD" },
	],
	organize: [{ id: "library", name: "Library", hint: "" }],
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
		{ id: "nx-merge", name: "Merge Switch NSP/XCI", hint: "→ super NSP/XCI" },
		{ id: "nx-split", name: "Split Switch NSP/XCI", hint: "per-title" },
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
const rowsEl = ref<HTMLElement | null>(null);

async function scrollActiveRow() {
	await nextTick();
	rowsEl.value?.querySelector(".row.active")?.scrollIntoView({ block: "nearest" });
}

onMounted(() => {
	scrollActiveRow();
});
watch(() => route.path, scrollActiveRow);

function pick(id: string) {
	ui.setLastConsole(props.op, id);
	router.push(`/${props.op}/${id}`);
}
</script>

<template>
	<aside class="panel">
		<div class="title">{{ TITLES[op] }}</div>
		<p class="subtitle">{{ SUBTITLES[op] }}</p>

		<div ref="rowsEl" class="rows">
			<button
				v-for="row in rows"
				:key="row.id"
				type="button"
				class="row"
				:class="{ active: activeConsole === row.id }"
				:title="row.name"
				@click="pick(row.id)"
			>
				<span class="name">{{ row.name }}</span>
				<span class="hint" :title="row.hint">{{ row.hint }}</span>
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
	width: clamp(212px, 17vw, 240px);
	flex-shrink: 0;
	min-height: 0;
	overflow-y: hidden;
	overflow-x: hidden;
	background: var(--bg3);
	border-right: 1px solid var(--a10);
	padding: 16px 10px 12px;
}
.title {
	flex: none;
	font-size: var(--fs-lg);
	font-weight: 700;
	color: var(--t0);
}
.subtitle {
	flex: none;
	margin: 6px 0 12px;
	font-size: var(--fs-sm);
	color: var(--t4);
	line-height: var(--lh-body);
	text-wrap: pretty;
}
.rows {
	display: flex;
	flex-direction: column;
	flex: 0 1 auto;
	min-height: 0;
	overflow-y: auto;
	padding-inline: 3px;
	margin-inline: -3px;
	scrollbar-width: thin;
	gap: 2px;
}
.row {
	display: flex;
	flex-wrap: wrap;
	flex: none;
	align-items: baseline;
	justify-content: space-between;
	column-gap: 8px;
	row-gap: 0;
	min-height: 32px;
	padding: 7px 10px;
	border: none;
	border-radius: var(--r-md);
	background: transparent;
	color: var(--t3);
	font-size: var(--fs-md);
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
	flex: 0 1 auto;
	min-width: 0;
	line-height: var(--lh-body);
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}
.hint {
	flex: 0 0 auto;
	margin-left: auto;
	line-height: var(--lh-body);
	font-family: var(--font-mono);
	font-size: var(--fs-xs);
	color: var(--t5);
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}
.row.active .hint {
	color: var(--blue);
}
.hint:empty {
	display: none;
}
.note {
	margin-top: 8px;
	font-size: var(--fs-sm);
	color: var(--t6);
	line-height: var(--lh-body);
	text-wrap: pretty;
}
.spacer {
	flex: 1;
}
@media (max-height: 719px) {
	.panel {
		padding-top: 12px;
	}
	.subtitle {
		display: none;
	}
	.rows {
		margin-top: 8px;
	}
	.row {
		min-height: 28px;
		padding-top: 4px;
		padding-bottom: 4px;
	}
}
</style>
