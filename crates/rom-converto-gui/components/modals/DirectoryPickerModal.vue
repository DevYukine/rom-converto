<script setup lang="ts">
import { computed } from "vue";
import ModalShell from "~/components/modals/ModalShell.vue";
import { open } from "~/lib/ipc";

const props = defineProps<{
	modelValue: string;
	defaultOutputDir: string;
}>();

const emit = defineEmits<{
	"update:modelValue": [value: string];
	close: [];
}>();

const rows = computed(() => [
	{ label: "same as source", value: "" },
	{ label: props.defaultOutputDir, value: props.defaultOutputDir },
	{ label: "~/roms/output", value: "~/roms/output" },
	{ label: "~/emulation/archive", value: "~/emulation/archive" },
]);

function select(value: string) {
	emit("update:modelValue", value);
	emit("close");
}

async function chooseFolder() {
	const picked = await open({ directory: true, multiple: false });
	if (typeof picked === "string" && picked) select(picked);
}
</script>

<template>
	<ModalShell title="Output directory" :width="460" @close="emit('close')">
		<div class="rc-rows">
			<button
				v-for="row in rows"
				:key="row.label"
				type="button"
				class="rc-row"
				@click="select(row.value)"
			>
				<span class="rc-icon">📁</span>
				<span class="rc-path">{{ row.label }}</span>
				<span v-if="row.value === modelValue" class="rc-check">✓</span>
			</button>
			<button type="button" class="rc-row" @click="chooseFolder">
				<span class="rc-icon">📁</span>
				<span class="rc-path">Choose another folder…</span>
			</button>
		</div>
	</ModalShell>
</template>

<style scoped>
.rc-rows {
	display: flex;
	flex-direction: column;
	gap: 2px;
}

.rc-row {
	display: flex;
	align-items: center;
	gap: 8px;
	width: 100%;
	background: none;
	border: none;
	border-radius: 8px;
	padding: 8px 10px;
	color: var(--t2);
	font-size: 12px;
	cursor: pointer;
	text-align: left;
}

.rc-row:hover {
	background: var(--a08);
}

.rc-icon {
	font-size: 13px;
}

.rc-path {
	flex: 1;
	font-family: ui-monospace, monospace;
	font-size: 11px;
	overflow: hidden;
	text-overflow: ellipsis;
	white-space: nowrap;
}

.rc-check {
	color: var(--green);
	font-weight: 700;
}
</style>
