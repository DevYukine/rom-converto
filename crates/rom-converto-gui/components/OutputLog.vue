<script setup lang="ts">
defineProps<{
  result: string;
  error: string;
}>();
</script>

<template>
  <div v-if="result || error" class="mt-4 rounded-lg border border-zinc-700 p-3">
    <p v-if="result" class="text-sm text-emerald-400">{{ result }}</p>
    <p v-if="error" class="text-sm text-red-400">{{ error }}</p>
  </div>
</template>
