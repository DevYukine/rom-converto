<script setup lang="ts">
import { computed, ref } from "vue";
import ModalShell from "~/components/modals/ModalShell.vue";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";

const props = withDefaults(
	defineProps<{
		modelValue: string;
		placeholder?: string;
	}>(),
	{ placeholder: "None" },
);

const emit = defineEmits<{
	"update:modelValue": [value: string];
	close: [];
}>();

// The per-file output template tokens of the backend's `util/template.rs`;
// organize's layout adds DAT and frontend tokens that have no value here.
const SAMPLE: Record<string, string> = {
	console: "switch",
	title: "Example Game",
	titleId: "0100000000000000",
	region: "World",
	serial: "LA-H-AAAAA",
	basename: "Example Game",
	ext: "nsz",
};
const TOKENS = Object.keys(SAMPLE).map((key) => `{${key}}`);

const text = ref(props.modelValue);

const preview = computed(() =>
	text.value.replace(/\{([A-Za-z]+)\}/g, (match, key: string) => SAMPLE[key] ?? match),
);

function update(value: string) {
	text.value = value;
	emit("update:modelValue", value);
}

function insert(token: string) {
	update(text.value + token);
}

function clear() {
	update("");
}
</script>

<template>
	<ModalShell title="Output template" :width="520" @close="emit('close')">
		<input
			class="rc-input rc-input--mono"
			type="text"
			spellcheck="false"
			:placeholder="placeholder"
			:value="text"
			@input="update(($event.target as HTMLInputElement).value)"
		/>
		<div class="rc-tokens">
			<span class="rc-tokens-label">Insert:</span>
			<button v-for="t in TOKENS" :key="t" type="button" class="rc-chip" @click="insert(t)">{{ t }}</button>
		</div>
		<div class="rc-preview">
			<span class="rc-preview__label">Preview: </span>
			<span v-if="text" class="rc-preview__value">{{ preview }}</span>
			<span v-else class="rc-preview__placeholder">{{ placeholder }}</span>
		</div>

		<template #footer>
			<button type="button" class="rc-link" @click="clear">Clear template</button>
			<div class="rc-footer-actions">
				<PrimaryButton @click="emit('close')">Done</PrimaryButton>
			</div>
		</template>
	</ModalShell>
</template>

<style scoped>
.rc-input {
	width: 100%;
}

.rc-tokens {
	display: flex;
	align-items: center;
	gap: 6px;
	margin-top: 10px;
	flex-wrap: wrap;
}

.rc-tokens-label {
	font-size: var(--fs-sm);
	color: var(--t5);
}

.rc-chip {
	flex: none;
	height: 26px;
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: var(--r-sm);
	padding: 0 8px;
	color: var(--t3);
	font-family: var(--font-mono);
	font-size: var(--fs-sm);
	cursor: pointer;
	white-space: nowrap;
}

.rc-chip:hover {
	border-color: var(--a30);
	color: var(--t0);
}

.rc-preview {
	margin-top: 12px;
	font-size: var(--fs-sm);
	line-height: var(--lh-body);
	overflow-wrap: anywhere;
}

.rc-preview__label {
	font-size: inherit;
	color: var(--t5);
}

.rc-preview__value {
	font-family: var(--font-mono);
	color: var(--green);
}

.rc-preview__placeholder {
	color: var(--t6);
}

.rc-link {
	background: none;
	border: none;
	border-radius: var(--r-sm);
	color: var(--blue);
	font-size: var(--fs-sm);
	min-height: var(--ctl-h);
	cursor: pointer;
	padding: 0 8px;
	white-space: nowrap;
}

.rc-link:hover {
	background: var(--a06);
}

.rc-footer-actions {
	margin-left: auto;
}
</style>
