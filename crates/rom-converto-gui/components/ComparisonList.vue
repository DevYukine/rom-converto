<script setup lang="ts">
import type { ComparisonSummary } from "~/types/report";

defineProps<{
  comparisons: ComparisonSummary[];
}>();
</script>

<template>
  <div v-if="comparisons.length" class="space-y-3">
    <h3 class="text-sm font-medium text-zinc-400">Comparison</h3>
    <ComparisonCard v-for="(comparison, i) in comparisons" :key="i" :comparison="comparison" />
  </div>
</template>
