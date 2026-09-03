<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { invoke, save } from "~/lib/ipc";
import { useToast } from "~/composables/useToast";
import { parseHashLine } from "~/lib/hash-lines";
import { contentTypeDisplayName } from "~/lib/display";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";
import ContentTypeChip from "~/components/ui/ContentTypeChip.vue";
import KvRow from "~/components/ui/KvRow.vue";
import InnerFilesList from "~/components/op/InnerFilesList.vue";
import {
	buildInspectView,
	englishFirst,
	formatBytes,
	formatMaker,
	formatXboxPartitionKind,
	pkgPlatformBadge,
	RETRO_SYSTEM_NAMES,
	retroTitle,
	xenonRatio,
} from "~/lib/inspect-view";
import { imageToDataUrl, pickBackgroundImage, pickIconImage, type InfoResult } from "~/types/info";

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
	xbox: "XBOX",
	xenon: "XBOX 360",
	ps3: "PS3",
	psx: "PS1",
	psp: "PSP",
	laser_disc: "LASERDISC",
	nds: "DS",
	retro: "RETRO",
	pbp: "PSP",
	vpk: "VITA",
	pkg: "VITA",
};

const view = computed(() => buildInspectView(props.info));

const iconUrl = computed(() => {
	const img = pickIconImage(props.info);
	return img ? imageToDataUrl(img) : null;
});

const iconCaption = computed(() => {
	if (props.info.kind === "nx" && !props.info.full) return "load prod.keys";
	// Vita pkg artwork sits behind PFS; it only decrypts with a license
	// (work.bin / .rif) next to the package.
	if (props.info.kind === "pkg" && props.info.platform === "vita") return "add work.bin";
	return "game icon";
});

const backgroundUrl = computed(() => {
	const img = pickBackgroundImage(props.info);
	return img ? imageToDataUrl(img) : null;
});

const sizeBytes = computed(() => {
	switch (props.info.kind) {
		case "wup":
			return props.info.total_content_size;
		case "xbox":
			return props.info.image_size;
		case "xenon":
			return props.info.compressed_size;
		case "ps3":
		case "psx":
		case "psp":
			return props.info.size_bytes;
		case "laser_disc":
			return props.info.file_size_bytes;
		case "retro":
			return props.info.file_size;
		case "vpk":
		case "pkg":
			return props.info.total_size;
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
		case "chd": {
			const fallback = info.version_string || `CHD v${info.version}`;
			if (info.content?.kind === "psp") return info.content.title || info.content.title_id || fallback;
			if (info.content?.kind === "psx") return info.content.volume_id || info.content.title_id || fallback;
			return fallback;
		}
		case "cso": {
			const fallback = `${info.format} image`;
			if (info.content?.kind === "psp") return info.content.title || info.content.title_id || fallback;
			if (info.content?.kind === "psx") return info.content.volume_id || info.content.title_id || fallback;
			return fallback;
		}
		case "xbox":
			return info.xbe?.title_name || info.xex?.title_name || `${formatXboxPartitionKind(info.partition_kind)} image`;
		case "xenon":
			return info.xex?.title_name || "Xbox 360 image";
		case "ps3":
			return info.title || info.title_id || "PS3 disc";
		case "psx":
			return info.volume_id || info.title_id || "PlayStation disc";
		case "psp":
			return info.title || info.title_id || "PSP disc";
		case "laser_disc":
			return "LaserDisc rip";
		case "nds":
			return englishFirst(info.banner?.titles.entries, (e) => e[0])?.[1] || info.game_title;
		case "retro":
			return retroTitle(info.details) || RETRO_SYSTEM_NAMES[info.details.system];
		case "pbp":
			return info.title || info.disc_id || "PSP image";
		case "vpk":
			return info.title || info.title_id || "Vita package";
		case "pkg":
			return info.title || info.title_id || "Vita package";
	}
});

// Raw disc images all read "DISC" regardless of the extension they came
// with (.iso, .gcm, .cue); compressed or archive containers keep their
// format name.
const RETRO_DISC_SYSTEMS = new Set(["sega_saturn", "sega_cd", "dreamcast"]);

const formatBadge = computed(() => {
	const info = props.info;
	switch (info.kind) {
		case "ctr":
			return info.format.toUpperCase();
		case "dol":
		case "rvl": {
			const container = info.container.toUpperCase();
			return container === "ISO" || container === "GCM" ? "DISC" : container;
		}
		case "wup":
			return info.source_kind.toUpperCase();
		case "nx":
			return info.container_kind.toUpperCase();
		case "chd":
			return "CHD";
		case "cso":
			return info.format.toUpperCase();
		case "xbox":
			return "DISC";
		case "xenon":
			return "ZAR";
		case "ps3":
			return "DISC";
		case "psx":
			return "DISC";
		case "psp":
			return "DISC";
		case "laser_disc":
			return "AVI";
		case "nds":
			return "NDS";
		case "retro":
			return RETRO_DISC_SYSTEMS.has(info.details.system) ? "DISC" : "ROM";
		case "pbp":
			return "EBOOT.PBP";
		case "vpk":
			return "VPK";
		case "pkg":
			return "PKG";
	}
});

const consoleBadge = computed(() => {
	const info = props.info;
	if (info.kind === "psx") return info.console;
	if (info.kind === "chd" || info.kind === "cso") {
		if (info.content?.kind === "psx") return info.content.console;
		if (info.content?.kind === "psp") return "PSP";
	}
	if (info.kind === "pkg") return pkgPlatformBadge(info.platform);
	if (info.kind === "retro") return RETRO_SYSTEM_NAMES[info.details.system].toUpperCase();
	return CONSOLE_LABEL[info.kind];
});

// Physical medium of disc-based inputs; null for cartridges and digital
// packages, which have no disc to describe.
const mediaBadge = computed(() => {
	const info = props.info;
	switch (info.kind) {
		case "psx":
			return info.media;
		case "psp":
			return "UMD";
		case "ps3":
			return "BD";
		case "dol":
			return "MiniDVD";
		case "rvl":
			return "DVD";
		case "xbox":
			return "DVD";
		case "chd":
			if (info.content?.kind === "psx") return info.content.media;
			if (info.content?.kind === "psp") return "UMD";
			if (info.ld) return "LaserDisc";
			if (info.dvd) return "DVD";
			return info.tracks.length ? "CD" : null;
		case "cso":
			if (info.content?.kind === "psx") return info.content.media;
			if (info.content?.kind === "psp") return "UMD";
			return null;
		case "laser_disc":
			return "LaserDisc";
		case "retro":
			switch (info.details.system) {
				case "sega_saturn":
				case "sega_cd":
					return "CD";
				case "dreamcast":
					return "GD-ROM";
				default:
					return null;
			}
		default:
			return null;
	}
});

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
			if (info.content) {
				if (info.content.title_id) parts.push(info.content.title_id);
				if (info.content.version) parts.push(`v${info.content.version}`);
			} else {
				parts.push(info.compressors.join(", "));
			}
			break;
		case "cso":
			if (info.content) {
				if (info.content.title_id) parts.push(info.content.title_id);
				if (info.content.version) parts.push(`v${info.content.version}`);
			} else {
				parts.push(`block ${info.block_size}`);
			}
			break;
		case "ps3":
			if (info.region) parts.push(info.region);
			if (info.version) parts.push(`v${info.version}`);
			break;
		case "psx":
			if (info.version) parts.push(`v${info.version}`);
			break;
		case "psp":
			if (info.firmware) parts.push(`fw ${info.firmware}`);
			if (info.content_kind) parts.push(contentTypeDisplayName(info.content_kind));
			else if (info.category) parts.push(info.category);
			break;
		case "laser_disc":
			parts.push(`${info.video_width}x${info.video_height}`, `${info.fps.toFixed(2)} fps`);
			break;
		case "nds":
			parts.push(info.maker_code, info.unit_code_name);
			break;
		case "pbp":
			if (info.content_kind) parts.push(contentTypeDisplayName(info.content_kind));
			else if (info.category_label ?? info.category) parts.push(info.category_label ?? info.category ?? "");
			if (info.disc_version) parts.push(`v${info.disc_version}`);
			break;
		case "vpk":
			if (info.content_kind) parts.push(contentTypeDisplayName(info.content_kind));
			else if (info.category_label ?? info.category) parts.push(info.category_label ?? info.category ?? "");
			if (info.app_ver) parts.push(`v${info.app_ver}`);
			break;
		case "pkg":
			if (info.content_kind) parts.push(contentTypeDisplayName(info.content_kind));
			else if (info.content_type_label ?? info.category) parts.push(info.content_type_label ?? info.category ?? "");
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
			if (info.tmd) stats.push({ label: "Title ID", value: info.tmd.title_id_hex });
			break;
		case "wup":
			stats.push({ label: "Title ID", value: info.title_id_hex });
			stats.push({ label: "Contents", value: String(info.content_count) });
			break;
		case "nx":
			if (info.full) stats.push({ label: "Title ID", value: info.full.application_title_id_hex });
			stats.push({ label: "NCA files", value: String(info.nca_names.length) });
			if (info.is_compressed) stats.push({ label: "Compressed", value: "zstd", color: "green" });
			break;
		case "chd":
			if (info.content?.title_id) stats.push({ label: "Title ID", value: info.content.title_id });
			stats.push({ label: "Ratio", value: `${info.compression_ratio.toFixed(1)}%`, color: "green" });
			stats.push({ label: "Hunks", value: String(info.hunk_count) });
			break;
		case "cso":
			if (info.content?.title_id) stats.push({ label: "Title ID", value: info.content.title_id });
			stats.push({ label: "Ratio", value: `${info.compression_ratio.toFixed(1)}%`, color: "green" });
			stats.push({ label: "Blocks", value: String(info.block_count) });
			break;
		case "xbox": {
			const titleIdHex = info.xbe?.title_id_hex ?? info.xex?.title_id_hex;
			if (titleIdHex) stats.push({ label: "Title ID", value: titleIdHex });
			stats.push({ label: "Partition", value: formatXboxPartitionKind(info.partition_kind) });
			stats.push({ label: "Files", value: String(info.file_count) });
			break;
		}
		case "xenon": {
			if (info.xex?.title_id_hex) stats.push({ label: "Title ID", value: info.xex.title_id_hex });
			stats.push({ label: "Ratio", value: `${xenonRatio(info.logical_size, info.compressed_size).toFixed(1)}%`, color: "green" });
			stats.push({ label: "Blocks", value: String(info.block_count) });
			break;
		}
		case "ps3":
			if (info.title_id) stats.push({ label: "Title ID", value: info.title_id });
			if (info.encrypted !== null) stats.push({ label: "Encryption", value: info.encrypted ? "encrypted" : "decrypted ✓" });
			break;
		case "nds":
			stats.push({ label: "Game Code", value: info.game_code });
			stats.push({
				label: "Encryption",
				value: info.secure_area === "not_present" ? "not present" : info.secure_area === "decrypted" ? "decrypted ✓" : "encrypted",
			});
			break;
		case "retro":
			stats.push({ label: "System", value: RETRO_SYSTEM_NAMES[info.details.system] });
			break;
		case "pbp":
			if (info.disc_id) stats.push({ label: "Disc ID", value: info.disc_id });
			stats.push({ label: "Segments", value: String(info.segments.filter((s) => s.present).length) });
			break;
		case "vpk":
			if (info.title_id) stats.push({ label: "Title ID", value: info.title_id });
			stats.push({ label: "Files", value: String(info.file_count) });
			break;
		case "pkg":
			if (info.title_id) stats.push({ label: "Title ID", value: info.title_id });
			stats.push({ label: "Items", value: String(info.item_count) });
			break;
	}
	return stats;
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

const canCopyTitleId = computed(() => {
	const info = props.info;
	if (info.kind === "chd" || info.kind === "cso" || info.kind === "laser_disc") return false;
	if (info.kind === "xbox") return !!(info.xbe || info.xex);
	if (info.kind === "xenon") return !!info.xex;
	if (info.kind === "retro") return false;
	return true;
});

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
			value = info.tmd ? info.tmd.title_id_hex : info.game_id;
			break;
		case "wup":
			value = info.title_id_hex;
			break;
		case "nx":
			value = info.full?.application_title_id_hex ?? "";
			break;
		case "ps3":
			value = info.title_id ?? "";
			break;
		case "psx":
			value = info.title_id ?? "";
			break;
		case "psp":
			value = info.title_id ?? "";
			break;
		case "xbox":
			value = info.xbe?.title_id_hex ?? info.xex?.title_id_hex ?? "";
			break;
		case "xenon":
			value = info.xex?.title_id_hex ?? "";
			break;
		case "nds":
			value = info.game_code;
			break;
		case "pbp":
			value = info.disc_id ?? "";
			break;
		case "vpk":
			value = info.title_id ?? "";
			break;
		case "pkg":
			value = info.title_id ?? "";
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
		<div v-if="backgroundUrl" class="rc-inspect-card__banner">
			<img :src="backgroundUrl" alt="" />
		</div>
		<div class="rc-inspect-card__top">
			<div class="rc-inspect-card__icon">
				<img v-if="iconUrl" :src="iconUrl" alt="" />
				<span v-else class="rc-inspect-card__icon-caption">{{ iconCaption }}</span>
			</div>

			<div class="rc-inspect-card__main">
				<div class="rc-inspect-card__title-row">
					<span class="rc-inspect-card__title">{{ title }}</span>
					<ContentTypeChip v-if="view.contentType" :type="view.contentType" />
					<span class="rc-inspect-card__badge rc-inspect-card__badge--console">{{ consoleBadge }}</span>
					<span class="rc-inspect-card__badge rc-inspect-card__badge--format">{{ formatBadge }}</span>
					<span v-if="mediaBadge" class="rc-inspect-card__badge rc-inspect-card__badge--media">{{ mediaBadge }}</span>
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
				<h4>Container</h4>
				<div v-if="view.container.length === 0" class="rc-inspect-card__empty">Not a container format.</div>
				<KvRow v-for="f in view.container" :key="f.label" :label="f.label" :value="f.value" />
			</div>
			<div class="rc-inspect-card__col">
				<h4>ROM</h4>
				<div v-if="view.rom.length === 0" class="rc-inspect-card__empty">
					No ROM metadata detected inside this container.
				</div>
				<KvRow v-for="f in view.rom" :key="f.label" :label="f.label" :value="f.value" />
			</div>
			<div class="rc-inspect-card__col">
				<InnerFilesList :title="view.innerTitle" :items="view.innerFiles" />
			</div>
			<div class="rc-inspect-card__col">
				<h4>Hashes</h4>
				<KvRow
					v-for="h in view.hashes"
					:key="h.label"
					:label="h.label"
					:value="h.value"
					clickable
					tooltip="Click to copy"
					@click="copyValue(h.value)"
				/>
				<KvRow
					v-for="h in computedHashes"
					:key="h.label"
					:label="h.label"
					:value="h.value"
					clickable
					tooltip="Click to copy"
					@click="copyValue(h.value)"
				/>
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

.rc-inspect-card__banner {
	max-height: 140px;
	overflow: hidden;
	border-radius: 10px 10px 0 0;
}

.rc-inspect-card__banner img {
	width: 100%;
	object-fit: cover;
	display: block;
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
	min-width: 0;
	overflow-wrap: anywhere;
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

.rc-inspect-card__badge--media {
	background: rgba(163, 113, 247, 0.16);
	color: var(--purple);
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
	grid-template-columns: repeat(2, minmax(0, 1fr));
	gap: 16px 24px;
	padding: 14px 16px;
}

@media (max-width: 900px) {
	.rc-inspect-card__grid {
		grid-template-columns: 1fr;
	}
}

.rc-inspect-card__col h4 {
	margin: 0 0 6px;
	font-size: 10.5px;
	font-weight: 700;
	text-transform: uppercase;
	letter-spacing: 0.8px;
	color: var(--t4);
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
	overflow-wrap: anywhere;
}

.rc-inspect-card__empty {
	font-size: 11.5px;
	color: var(--t5);
	margin: 4px 0 0;
}
</style>
