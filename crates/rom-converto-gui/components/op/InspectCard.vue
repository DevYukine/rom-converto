<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { invoke, save } from "~/lib/ipc";
import { useToast } from "~/composables/useToast";
import { parseHashLine } from "~/lib/hash-lines";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";
import { imageToDataUrl, nxTitleKindDisplayName, pickIconImage, type InfoResult } from "~/types/info";

const props = defineProps<{
	info: InfoResult;
	rawJson: string;
	path: string;
	canCompress: boolean;
	canVerify: boolean;
}>();

const emit = defineEmits<{ compress: []; verify: [] }>();

const { show: showToast } = useToast();

const CONSOLE_LABEL: Record<InfoResult["kind"], string> = {
	ctr: "3DS",
	dol: "GAMECUBE",
	rvl: "WII",
	wup: "WII U",
	nx: "SWITCH",
	chd: "CHD",
	cso: "CSO",
};

const iconUrl = computed(() => {
	const img = pickIconImage(props.info);
	return img ? imageToDataUrl(img) : null;
});

function formatMaker(code: string, name: string | null): string {
	return name ? `${code} (${name})` : code;
}

// Language tags differ per format ("AmericanEnglish", "english", "american_english");
// normalize before comparing. Falls back to the first entry.
function englishFirst<T>(items: T[] | undefined, lang: (item: T) => string): T | undefined {
	if (!items?.length) return undefined;
	for (const pref of ["americanenglish", "english", "britishenglish"]) {
		const hit = items.find((item) => lang(item).replace(/[_\s]/g, "").toLowerCase() === pref);
		if (hit) return hit;
	}
	return items[0];
}

function hex16(n: number): string {
	return n.toString(16).padStart(16, "0").toUpperCase();
}

function formatBytes(n: number): string {
	if (n < 1024) return `${n} B`;
	const units = ["KiB", "MiB", "GiB", "TiB"];
	let value = n / 1024;
	let unit = 0;
	while (value >= 1024 && unit < units.length - 1) {
		value /= 1024;
		unit += 1;
	}
	return `${value.toFixed(value >= 10 ? 0 : 1)} ${units[unit]}`;
}

const sizeBytes = computed(() => {
	switch (props.info.kind) {
		case "wup":
			return props.info.total_content_size;
		default:
			return props.info.physical_bytes;
	}
});

const title = computed(() => {
	const info = props.info;
	switch (info.kind) {
		case "ctr":
			return (
				englishFirst(info.smdh?.titles, (t) => t.language)?.long_description ||
				info.product_code ||
				info.title_id
			);
		case "dol": {
			const t = englishFirst(info.banner?.titles, (b) => b.language);
			return t?.long_game_name || t?.short_game_name || info.game_name || info.game_id;
		}
		case "rvl":
			return (
				englishFirst(info.imet_names?.entries, (e) => e[0])?.[1] ||
				info.game_name ||
				info.game_id
			);
		case "wup":
			return englishFirst(info.meta?.long_names?.entries, (e) => e[0])?.[1] || info.title_id_hex;
		case "nx":
			return englishFirst(info.full?.control?.titles, (t) => t.language)?.name || info.container_kind.toUpperCase();
		case "chd":
			return info.version_string || `CHD v${info.version}`;
		case "cso":
			return `${info.format} image`;
	}
});

const formatBadge = computed(() => {
	const info = props.info;
	switch (info.kind) {
		case "ctr":
			return info.format.toUpperCase();
		case "dol":
			return info.container.toUpperCase();
		case "rvl":
			return info.container.toUpperCase();
		case "wup":
			return info.source_kind.toUpperCase();
		case "nx":
			return info.container_kind.toUpperCase();
		case "chd":
			return "CHD";
		case "cso":
			return info.format.toUpperCase();
	}
});

const consoleBadge = computed(() => CONSOLE_LABEL[props.info.kind]);

const metaLine = computed(() => {
	const info = props.info;
	const parts: string[] = [];
	switch (info.kind) {
		case "ctr":
			parts.push(formatMaker(info.maker_code, info.maker_name));
			if (info.smdh?.region_names?.length) parts.push(info.smdh.region_names.join(", "));
			break;
		case "dol":
			parts.push(formatMaker(info.maker_code, info.maker_name), info.region);
			break;
		case "rvl":
			parts.push(formatMaker(info.maker_code, info.maker_name), info.region);
			break;
		case "wup": {
			const pub = englishFirst(info.meta?.publishers?.entries, (e) => e[0])?.[1];
			if (pub) parts.push(pub);
			if (info.meta?.region_names?.length) parts.push(info.meta.region_names.join(", "));
			break;
		}
		case "nx": {
			const ctrl = info.full?.control;
			const pub = englishFirst(ctrl?.titles, (t) => t.language)?.publisher;
			if (pub) parts.push(pub);
			if (ctrl?.display_version) parts.push(`v${ctrl.display_version}`);
			break;
		}
		case "chd":
			parts.push(info.compressors.join(", "));
			break;
		case "cso":
			parts.push(`block ${info.block_size}`);
			break;
	}
	return parts.filter(Boolean).join(" · ");
});

interface Stat {
	label: string;
	value: string;
	color?: "t3" | "blue" | "green" | "yellow";
}

const statRow = computed<Stat[]>(() => {
	const info = props.info;
	const stats: Stat[] = [{ label: "Size", value: formatBytes(sizeBytes.value) }];
	switch (info.kind) {
		case "ctr":
			stats.push({ label: "Title ID", value: info.title_id });
			stats.push({ label: "Encryption", value: info.ncch_encrypted ? "encrypted" : "decrypted ✓" });
			if (info.compressed) stats.push({ label: "Compressed", value: "zstd" });
			break;
		case "dol":
			stats.push({ label: "Game ID", value: info.game_id });
			stats.push({ label: "Disc", value: `#${info.disc_number} v${info.disc_version}` });
			break;
		case "rvl":
			stats.push({ label: "Game ID", value: info.game_id });
			if (info.tmd) stats.push({ label: "Title ID", value: hex16(info.tmd.title_id) });
			break;
		case "wup":
			stats.push({ label: "Title ID", value: info.title_id_hex });
			stats.push({ label: "Contents", value: String(info.content_count) });
			break;
		case "nx":
			if (info.full) stats.push({ label: "Title ID", value: hex16(info.full.application_title_id) });
			stats.push({ label: "NCA files", value: String(info.nca_names.length) });
			if (info.is_compressed) stats.push({ label: "Compressed", value: "zstd", color: "green" });
			break;
		case "chd":
			stats.push({ label: "Ratio", value: `${info.compression_ratio.toFixed(1)}%`, color: "green" });
			stats.push({ label: "Hunks", value: String(info.hunk_count) });
			break;
		case "cso":
			stats.push({ label: "Ratio", value: `${info.compression_ratio.toFixed(1)}%`, color: "green" });
			stats.push({ label: "Blocks", value: String(info.block_count) });
			break;
	}
	return stats;
});

interface ContentRow {
	name: string;
	detail: string;
}

const contents = computed<ContentRow[]>(() => {
	const info = props.info;
	switch (info.kind) {
		case "nx":
			return info.files.map((f) => ({
				name: f.name,
				detail: f.partition ? `${formatBytes(f.size)} · ${f.partition}` : formatBytes(f.size),
			}));
		case "wup":
			return info.bundled_titles.map((b) => ({
				name: b.title_type,
				detail: `${b.title_id_hex} · v${b.title_version}`,
			}));
		case "rvl":
			return info.partitions.map((p) => ({
				name: p.kind,
				detail: `group ${p.group} · 0x${p.offset.toString(16).toUpperCase()}`,
			}));
		case "chd":
			return info.tracks.map((t) => ({
				name: `Track ${t.number}`,
				detail: `${t.track_type} · ${t.frames} frames`,
			}));
		default:
			return [];
	}
});

const details = computed<Stat[]>(() => {
	const info = props.info;
	const rows: Stat[] = [];
	const add = (label: string, value: string | number | null | undefined) => {
		if (value === null || value === undefined || value === "") return;
		rows.push({ label, value: String(value) });
	};
	switch (info.kind) {
		case "ctr":
			add("Format", info.format.toUpperCase());
			add("Program ID", info.program_id);
			add("Product code", info.product_code);
			add("Maker", formatMaker(info.maker_code, info.maker_name));
			if (info.cartridge_size) add("Cartridge", formatBytes(info.cartridge_size));
			if (info.smdh) {
				if (info.smdh.region_names.length) add("Region", info.smdh.region_names.join(", "));
				add("EULA", `v${info.smdh.eula_version_major}.${info.smdh.eula_version_minor}`);
				if (info.smdh.age_ratings.length) {
					add("Age ratings", info.smdh.age_ratings.map((r) => `${r.region} ${r.age}`).join(", "));
				}
			}
			break;
		case "dol":
			add("Container", info.container.toUpperCase());
			add("Region", info.region);
			add("Maker", formatMaker(info.maker_code, info.maker_name));
			add("Disc", `#${info.disc_number} v${info.disc_version}`);
			add("Apploader", info.apploader_date);
			add("Audio streaming", info.audio_streaming ? "yes" : "no");
			break;
		case "rvl":
			add("Container", info.container.toUpperCase());
			add("Region", info.region);
			add("Maker", formatMaker(info.maker_code, info.maker_name));
			add("Disc", `#${info.disc_number} v${info.disc_version}`);
			if (info.tmd) {
				add("Title version", `v${info.tmd.title_version}`);
				if (info.tmd.ios_slot != null) add("IOS", `IOS${info.tmd.ios_slot}`);
				add("TMD region", info.tmd.region_name);
				add("TMD contents", info.tmd.content_count);
			}
			break;
		case "wup": {
			add("Title type", info.title_type);
			add("Title version", `v${info.title_version}`);
			add("Contents", `${info.content_count} · ${formatBytes(info.total_content_size)}`);
			if (info.os_version != null) add("OS version", info.os_version);
			if (info.update_version != null) add("Update", `v${info.update_version}`);
			const m = info.meta;
			if (m) {
				add("Product code", m.product_code);
				add("Company", m.company_name ?? m.company_code);
				if (m.region_names.length) add("Region", m.region_names.join(", "));
				add("Mastered", m.mastering_date);
				if (m.save_size) add("Save size", formatBytes(m.save_size));
			}
			break;
		}
		case "nx": {
			add("Container", info.container_kind.toUpperCase());
			add("Distribution", info.distribution);
			add("Structure", info.structure);
			add("NCAs", `${info.nca_names.length} (${info.cnmt_nca_names.length} meta)`);
			if (info.tickets.length) add("Tickets", info.tickets.length);
			const f = info.full;
			if (f) {
				add("Kind", nxTitleKindDisplayName(f.title_kind));
				add("Title version", `v${f.title_version}`);
				if (f.base_application_id != null) add("Base title", hex16(f.base_application_id));
				add("Contents", `${f.content_count} · ${formatBytes(f.total_content_size)}`);
				const c = f.control;
				if (c) {
					add("Display version", c.display_version);
					if (c.supported_languages.length) add("Languages", c.supported_languages.join(", "));
					if (c.age_ratings.length) {
						add("Age ratings", c.age_ratings.map((r) => `${r.organization} ${r.age}`).join(", "));
					}
				}
			}
			break;
		}
		case "chd":
			add("CHD version", info.version);
			add("Codecs", info.compressors.join(", "));
			add("Hunk", `${formatBytes(info.hunk_bytes)} × ${info.hunk_count}`);
			add("Unit", formatBytes(info.unit_bytes));
			add("Logical size", formatBytes(info.logical_bytes));
			if (info.dvd) add("DVD", `${info.dvd.total_sectors} sectors · ${info.dvd.layer_class}`);
			if (info.metadata_tags.length) add("Metadata", info.metadata_tags.map((t) => t.tag).join(", "));
			break;
		case "cso":
			add("Format", `${info.format} v${info.version}`);
			add("Block size", formatBytes(info.block_size));
			add("Blocks", `${info.block_count} (${info.raw_block_count} raw)`);
			add("Index shift", info.index_shift);
			add("Uncompressed", formatBytes(info.uncompressed_size));
			break;
	}
	return rows;
});

const hashes = computed<Stat[]>(() => {
	const info = props.info;
	if (info.kind !== "chd") return [];
	const rows: Stat[] = [
		{ label: "Raw SHA-1", value: info.raw_sha1 },
		{ label: "SHA-1", value: info.sha1 },
	];
	if (info.parent_sha1) rows.push({ label: "Parent SHA-1", value: info.parent_sha1 });
	return rows;
});

const computedHashes = ref<Stat[]>([]);
const hashing = ref(false);
const hashError = ref("");

watch(
	() => props.path,
	() => {
		computedHashes.value = [];
		hashError.value = "";
		hashing.value = false;
	},
);

async function computeHashes() {
	const path = props.path;
	hashing.value = true;
	hashError.value = "";
	try {
		const text = await invoke<string>("cmd_hash", {
			input: path,
			algos: ["crc32", "md5", "sha1", "sha256"],
			recursive: false,
			maxDepth: null,
		});
		if (path !== props.path) return;
		const row = text.split("\n").map(parseHashLine).find(Boolean);
		computedHashes.value = row ? row.values : [];
		if (!computedHashes.value.length) hashError.value = "No hash data returned.";
	} catch (e) {
		if (path === props.path) hashError.value = String(e);
	} finally {
		if (path === props.path) hashing.value = false;
	}
}

async function copyValue(value: string) {
	try {
		await navigator.clipboard.writeText(value);
	} catch {
		// clipboard unavailable (permission denied or no secure context); nothing to fall back to.
	}
	showToast("Copied");
}

const canCopyTitleId = computed(() => props.info.kind !== "chd" && props.info.kind !== "cso");

function copyTitleId() {
	const info = props.info;
	let value = "";
	switch (info.kind) {
		case "ctr":
			value = info.title_id;
			break;
		case "dol":
			value = info.game_id;
			break;
		case "rvl":
			value = info.tmd ? hex16(info.tmd.title_id) : info.game_id;
			break;
		case "wup":
			value = info.title_id_hex;
			break;
		case "nx":
			value = info.full ? hex16(info.full.application_title_id) : "";
			break;
		default:
			return;
	}
	if (!value) return;
	navigator.clipboard?.writeText(value).then(() => showToast("Copied"));
}

async function saveIcon() {
	const dest = await save({ defaultPath: "icon.png", filters: [{ name: "PNG", extensions: ["png"] }] });
	if (!dest) return;
	await invoke("cmd_save_icon", { infoJson: props.rawJson, dest });
	showToast("Icon saved");
}
</script>

<template>
	<div class="rc-inspect-card">
		<div class="rc-inspect-card__top">
			<div class="rc-inspect-card__icon">
				<img v-if="iconUrl" :src="iconUrl" alt="" />
				<span v-else class="rc-inspect-card__icon-caption">game icon</span>
			</div>

			<div class="rc-inspect-card__main">
				<div class="rc-inspect-card__title-row">
					<span class="rc-inspect-card__title">{{ title }}</span>
					<span class="rc-inspect-card__badge rc-inspect-card__badge--format">{{ formatBadge }}</span>
					<span class="rc-inspect-card__badge rc-inspect-card__badge--console">{{ consoleBadge }}</span>
				</div>
				<div v-if="metaLine" class="rc-inspect-card__meta">{{ metaLine }}</div>
				<div class="rc-inspect-card__stats">
					<span v-for="s in statRow" :key="s.label" class="rc-inspect-card__stat">
						{{ s.label }} <b :class="s.color ? `rc-inspect-card__stat-v--${s.color}` : ''">{{ s.value }}</b>
					</span>
				</div>
			</div>

			<div class="rc-inspect-card__actions">
				<PrimaryButton v-if="canCompress" @click="emit('compress')">Compress this</PrimaryButton>
				<PrimaryButton v-if="canVerify" variant="outlined" @click="emit('verify')">Verify this</PrimaryButton>
				<button v-if="canCopyTitleId" type="button" class="rc-inspect-card__link" @click="copyTitleId">Copy title ID</button>
				<button v-if="iconUrl" type="button" class="rc-inspect-card__link" @click="saveIcon">Save icon</button>
			</div>
		</div>

		<div class="rc-inspect-card__grid">
			<div class="rc-inspect-card__col">
				<h4>Details</h4>
				<div v-if="details.length === 0" class="rc-inspect-card__empty">No further details in this metadata.</div>
				<div v-for="d in details" :key="d.label" class="rc-inspect-card__row">
					<span class="rc-inspect-card__row-name">{{ d.label }}</span>
					<span class="rc-inspect-card__row-detail" :title="d.value">{{ d.value }}</span>
				</div>
			</div>
			<div class="rc-inspect-card__col">
				<h4>Contents</h4>
				<div v-if="contents.length === 0" class="rc-inspect-card__empty">No inner file listing for this format.</div>
				<div v-for="(c, i) in contents" :key="`${c.name}-${i}`" class="rc-inspect-card__row">
					<span class="rc-inspect-card__row-name" :title="c.name">{{ c.name }}</span>
					<span class="rc-inspect-card__row-detail">{{ c.detail }}</span>
				</div>
			</div>
			<div class="rc-inspect-card__col">
				<h4>Hashes</h4>
				<div v-for="h in hashes" :key="h.label" class="rc-inspect-card__row">
					<span class="rc-inspect-card__row-name rc-inspect-card__row-name--fixed">{{ h.label }}</span>
					<button type="button" class="rc-inspect-card__row-detail rc-inspect-card__row-copy" :title="`${h.value} · click to copy`" @click="copyValue(h.value)">{{ h.value }}</button>
				</div>
				<div v-for="h in computedHashes" :key="h.label" class="rc-inspect-card__row">
					<span class="rc-inspect-card__row-name rc-inspect-card__row-name--fixed">{{ h.label }}</span>
					<button type="button" class="rc-inspect-card__row-detail rc-inspect-card__row-copy" :title="`${h.value} · click to copy`" @click="copyValue(h.value)">{{ h.value }}</button>
				</div>
				<div v-if="hashError" class="rc-inspect-card__error">{{ hashError }}</div>
				<button
					v-if="computedHashes.length === 0"
					type="button"
					class="rc-inspect-card__hash-btn"
					:disabled="hashing"
					@click="computeHashes"
				>
					{{ hashing ? "Hashing…" : "Compute CRC32 / MD5 / SHA-1 / SHA-256" }}
				</button>
				<p v-if="computedHashes.length === 0 && !hashing" class="rc-inspect-card__empty">
					Streams the whole file once; large images take a moment.
				</p>
			</div>
		</div>
	</div>
</template>

<style scoped>
.rc-inspect-card {
	border: 1px solid var(--a10);
	border-radius: 10px;
	background: var(--card);
}

.rc-inspect-card__top {
	display: flex;
	align-items: flex-start;
	gap: 14px;
	padding: 16px;
	border-bottom: 1px solid var(--a10);
}

.rc-inspect-card__icon {
	flex-shrink: 0;
	width: 86px;
	height: 86px;
	border: 1px solid var(--a18);
	border-radius: 12px;
	background: repeating-linear-gradient(45deg, var(--check1), var(--check1) 6px, var(--check2) 6px, var(--check2) 12px);
	display: flex;
	align-items: center;
	justify-content: center;
	overflow: hidden;
}

.rc-inspect-card__icon img {
	width: 100%;
	height: 100%;
	object-fit: contain;
	image-rendering: pixelated;
}

.rc-inspect-card__icon-caption {
	font-size: 9px;
	font-family: ui-monospace, monospace;
	color: var(--t5);
}

.rc-inspect-card__main {
	flex: 1;
	min-width: 0;
}

.rc-inspect-card__title-row {
	display: flex;
	align-items: center;
	gap: 8px;
	flex-wrap: wrap;
}

.rc-inspect-card__title {
	font-size: 17px;
	font-weight: 700;
	color: var(--t0);
}

.rc-inspect-card__badge {
	font-size: 10px;
	font-weight: 700;
	padding: 2px 7px;
	border-radius: 5px;
	letter-spacing: 0.4px;
}

.rc-inspect-card__badge--format {
	background: rgba(93, 148, 245, 0.16);
	color: var(--blue);
}

.rc-inspect-card__badge--console {
	background: var(--a10);
	color: var(--t3);
}

.rc-inspect-card__meta {
	margin-top: 4px;
	font-size: 12px;
	color: var(--t4);
}

.rc-inspect-card__stats {
	margin-top: 8px;
	display: flex;
	flex-wrap: wrap;
	gap: 14px;
	font-size: 11.5px;
	color: var(--t4);
}

.rc-inspect-card__stat b {
	color: var(--t2);
	font-weight: 600;
}

.rc-inspect-card__stat-v--blue {
	color: var(--blue) !important;
}
.rc-inspect-card__stat-v--green {
	color: var(--green) !important;
}
.rc-inspect-card__stat-v--yellow {
	color: var(--yellow) !important;
}

.rc-inspect-card__actions {
	flex-shrink: 0;
	display: flex;
	flex-direction: column;
	align-items: stretch;
	gap: 6px;
}

.rc-inspect-card__link {
	background: none;
	border: none;
	color: var(--blue);
	font-size: 11px;
	cursor: pointer;
	padding: 0;
	text-align: center;
}

.rc-inspect-card__grid {
	display: grid;
	grid-template-columns: repeat(auto-fit, minmax(230px, 1fr));
	gap: 16px;
	padding: 14px 16px;
}

.rc-inspect-card__col h4 {
	margin: 0 0 6px;
	font-size: 10.5px;
	font-weight: 700;
	text-transform: uppercase;
	letter-spacing: 0.8px;
	color: var(--t4);
}

.rc-inspect-card__row {
	display: flex;
	align-items: center;
	justify-content: space-between;
	gap: 10px;
	padding: 3px 0;
}

.rc-inspect-card__row-name {
	font-family: ui-monospace, monospace;
	font-size: 11.5px;
	color: var(--t3);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-inspect-card__row-name--fixed {
	flex-shrink: 0;
}

.rc-inspect-card__row-detail {
	min-width: 0;
	font-family: ui-monospace, monospace;
	font-size: 11px;
	color: var(--t4);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
	text-align: right;
}

.rc-inspect-card__row-copy {
	background: none;
	border: none;
	padding: 0;
	cursor: pointer;
}

.rc-inspect-card__row-copy:hover {
	color: var(--blue);
}

.rc-inspect-card__hash-btn {
	margin-top: 4px;
	border: 1px solid var(--a25);
	border-radius: 6px;
	padding: 5px 10px;
	font-size: 11px;
	color: var(--t0);
	font-weight: 500;
	background: transparent;
	cursor: pointer;
}

.rc-inspect-card__hash-btn:disabled {
	color: var(--t5);
	cursor: wait;
}

.rc-inspect-card__error {
	font-size: 11px;
	color: var(--red);
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-inspect-card__empty {
	font-size: 11.5px;
	color: var(--t5);
	margin: 4px 0 0;
}
</style>
