<script setup lang="ts">
import { computed } from "vue";
import { opDef } from "~/lib/opdefs";
import OpPage from "~/components/op/OpPage.vue";
import BundleView from "~/components/op/BundleView.vue";

const route = useRoute();

const op = computed(() => String(route.params.op ?? ""));
const consoleId = computed(() => String(route.params.console ?? ""));
const def = computed(() => opDef(op.value, consoleId.value));
const isBundle = computed(() => op.value === "compress" && consoleId.value === "wup");
</script>

<template>
	<BundleView v-if="def && isBundle" :key="`${op}/${consoleId}`" :def="def" />
	<OpPage v-else-if="def" :key="`${op}/${consoleId}`" :def="def" />
	<div v-else class="rc-missing">
		<h1>Unknown operation</h1>
		<p>{{ op }} / {{ consoleId }}</p>
	</div>
</template>

<style scoped>
.rc-missing {
	padding: 40px;
	color: var(--t4);
}

.rc-missing h1 {
	margin: 0 0 6px;
	font-size: 18px;
	color: var(--t0);
}
</style>
