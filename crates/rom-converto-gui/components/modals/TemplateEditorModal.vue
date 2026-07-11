<script setup lang="ts">
import { computed, ref } from "vue";
import ModalShell from "~/components/modals/ModalShell.vue";

const props = defineProps<{
	modelValue: string;
}>();

const emit = defineEmits<{
	"update:modelValue": [value: string];
	close: [];
}>();

const DEFAULT_TEMPLATE = "{console}/{title}.{ext}";
const TOKENS = ["{console}", "{title}", "{ext}", "{region}", "{titleid}"];
const SAMPLE: Record<string, string> = {
	console: "switch",
	title: "Example Game",
	ext: "nsz",
	region: "World",
	titleid: "0100000000000000",
};

const text = ref(props.modelValue);

const preview = computed(() =>
	text.value.replace(/\{(console|title|ext|region|titleid)\}/g, (_, key: string) => SAMPLE[key] ?? `{${key}}`),
);

function update(value: string) {
	text.value = value;
	emit("update:modelValue", value);
}

function insert(token: string) {
	update(text.value + token);
}

function resetDefault() {
	update(DEFAULT_TEMPLATE);
}
</script>

<template>
	<ModalShell title="Output template" :width="520" @close="emit('close')">
		<input
			class="rc-input"
			type="text"
			spellcheck="false"
			:value="text"
			@input="update(($event.target as HTMLInputElement).value)"
		/>
		<div class="rc-tokens">
			<span class="rc-tokens-label">Insert:</span>
			<button v-for="t in TOKENS" :key="t" type="button" class="rc-chip" @click="insert(t)">{{ t }}</button>
		</div>
		<div class="rc-preview">Preview: {{ preview }}</div>

		<template #footer>
			<button type="button" class="rc-link" @click="resetDefault">Reset to default</button>
			<div class="rc-spacer" />
			<button type="button" class="rc-primary" @click="emit('close')">Done</button>
		</template>
	</ModalShell>
</template>

<style scoped>
.rc-input {
	width: 100%;
	box-sizing: border-box;
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: 6px;
	padding: 7px 10px;
	color: var(--t1);
	font-family: ui-monospace, monospace;
	font-size: 12px;
}

.rc-input:focus-visible {
	outline: 2px solid var(--blue);
	outline-offset: 1px;
}

.rc-tokens {
	display: flex;
	align-items: center;
	gap: 6px;
	margin-top: 10px;
	flex-wrap: wrap;
}

.rc-tokens-label {
	font-size: 10.5px;
	color: var(--t5);
}

.rc-chip {
	background: var(--bg2);
	border: 1px solid var(--a14);
	border-radius: 6px;
	padding: 3px 8px;
	color: var(--t3);
	font-family: ui-monospace, monospace;
	font-size: 10.5px;
	cursor: pointer;
}

.rc-chip:hover {
	border-color: var(--a30);
	color: var(--t0);
}

.rc-preview {
	margin-top: 12px;
	font-family: ui-monospace, monospace;
	font-size: 11px;
	color: var(--green);
}

.rc-link {
	background: none;
	border: none;
	color: var(--blue);
	font-size: 11px;
	cursor: pointer;
	padding: 0;
}

.rc-spacer {
	flex: 1;
}

.rc-primary {
	background: #2f6fd0;
	color: #fff;
	font-weight: 700;
	border: none;
	border-radius: 8px;
	padding: 7px 16px;
	font-size: 12.5px;
	cursor: pointer;
}

.rc-primary:hover {
	background: #3b82f6;
}
</style>
