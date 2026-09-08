<script setup lang="ts">
import { computed, ref, watch } from "vue";
import { invoke, save } from "~/lib/ipc";
import { useToast } from "~/composables/useToast";
import { digestValues } from "~/lib/display";
import { runArgs } from "~/lib/opdefs/types";
import type { InfoResult, RunOutcome } from "~/types";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";
import ContentTypeChip from "~/components/ui/ContentTypeChip.vue";
import KvRow from "~/components/ui/KvRow.vue";
import InnerFilesList from "~/components/op/InnerFilesList.vue";
import { buildInspectView, formatBytes, moduleFor } from "~/lib/inspect-view";
import type { Stat } from "~/lib/inspect-view";
import { imageToDataUrl, pickBackgroundImage, pickIconImage } from "~/lib/info";

const props = defineProps<{
	info: InfoResult;
	rawJson: string;
	path: string;
	canCompress: boolean;
	canVerify: boolean;
}>();

const emit = defineEmits<{ compress: []; verify: [] }>();

const { show: showToast } = useToast();

// Everything console-specific lives in the kind module; this card only lays it out.
const mod = computed(() => moduleFor(props.info));

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

const title = computed(() => mod.value.title(props.info));
const formatBadge = computed(() => mod.value.format(props.info));
const consoleBadge = computed(() => mod.value.console(props.info));
const mediaBadge = computed(() => mod.value.media?.(props.info) ?? null);
const metaLine = computed(() => (mod.value.meta?.(props.info) ?? []).filter(Boolean).join(" · "));

const statRow = computed<Stat[]>(() => [
	{ label: "Size", value: formatBytes(mod.value.size(props.info)) },
	...(mod.value.stats?.(props.info) ?? []),
]);

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
		const res = await invoke<RunOutcome>(
			"cmd_run",
			runArgs("hash", path, null, { algo: "crc32,md5,sha1,sha256" }, false, "inspect-hash"),
		);
		if (path !== props.path) return;
		computedHashes.value = digestValues(res.data);
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

// null means the kind has no title ID to offer; an empty string means it has
// one in principle but not in this file.
const titleIdValue = computed(() => mod.value.titleId?.(props.info) ?? null);
const canCopyTitleId = computed(() => titleIdValue.value !== null);

function copyTitleId() {
	const value = titleIdValue.value;
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
