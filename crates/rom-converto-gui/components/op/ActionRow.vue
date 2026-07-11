<script setup lang="ts">
import { computed, ref } from "vue";
import { invoke } from "~/lib/ipc";
import { useQueueStore } from "~/stores/queue";
import { buildCliCommand } from "~/composables/useCliEcho";
import PrimaryButton from "~/components/ui/PrimaryButton.vue";
import DryRunModal from "~/components/modals/DryRunModal.vue";
import type { DryRunLine } from "~/components/modals/DryRunModal.vue";
import { opCommand, opProgressKey } from "~/lib/opdefs/types";
import type { OpDef, OpStore, StagedItem } from "~/lib/opdefs/types";

const props = defineProps<{
	def: OpDef;
	store: OpStore;
	items: StagedItem[];
}>();

const emit = defineEmits<{ enqueued: [] }>();

const queue = useQueueStore();

const count = computed(() => props.items.length);
const label = computed(() => (count.value > 0 ? `Add ${count.value} to queue` : "Nothing staged"));

function taskIdFor(): string {
	return opProgressKey(props.def, props.store) ?? `job-${crypto.randomUUID()}`;
}

function enqueue() {
	if (!count.value) return;
	const groupId = props.store.reportFile ? crypto.randomUUID() : undefined;
	const specs = props.items.map((item) => {
		const taskId = taskIdFor();
		return {
			name: item.name,
			opLabel: props.def.opLabel,
			command: opCommand(props.def, props.store),
			args: props.def.buildArgs(props.store, item, taskId),
			taskId,
			chips: props.def.chips(props.store),
			resultKind: props.def.resultKind,
			routeBack: { storeId: props.def.storeId },
			groupId,
			inputBytes: item.size,
		};
	});
	queue.enqueue(specs);
	emit("enqueued");
}

const dryLines = ref<DryRunLine[]>([]);
const dryCommand = ref("");
const dryOpen = ref(false);

async function dryRun() {
	if (!count.value) return;
	const lines: DryRunLine[] = [];
	let cmd = "";
	for (const item of props.items) {
		const args = props.def.buildArgs(props.store, item, taskIdFor());
		const command = opCommand(props.def, props.store);
		if (!cmd) cmd = buildCliCommand(command, args, props.def.console);
		let note = "ok";
		let conflict = false;
		try {
			const res = await invoke<{ message?: string }>(command, { ...args, dryRun: true });
			const msg = typeof res === "object" && res ? String(res.message ?? "") : String(res);
			if (msg) note = msg;
			conflict = /exists|rename/i.test(msg);
		} catch (e) {
			note = String(e);
			conflict = true;
		}
		lines.push({
			source: item.name,
			output: props.def.deriveOutput ? props.def.deriveOutput(item.path, props.store) : item.path,
			note,
			conflict,
		});
	}
	dryLines.value = lines;
	dryCommand.value = cmd;
	dryOpen.value = true;
}
</script>

<template>
	<div class="rc-actions">
		<PrimaryButton :disabled="count === 0" @click="enqueue">{{ label }}</PrimaryButton>
		<PrimaryButton v-if="def.showDryRun !== false" variant="outlined" :disabled="count === 0" @click="dryRun">
			Dry run
		</PrimaryButton>
		<span class="rc-actions__note">{{ def.actionNote }}</span>

		<DryRunModal v-if="dryOpen" :command="dryCommand" :lines="dryLines" @close="dryOpen = false" />
	</div>
</template>

<style scoped>
.rc-actions {
	display: flex;
	align-items: center;
	gap: 12px;
}

.rc-actions__note {
	font-size: 11.5px;
	color: var(--t4);
}
</style>
