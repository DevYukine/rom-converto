<script setup lang="ts">
defineProps<{
  result?: string;
  error?: string;
}>();
</script>

<template>
  <div v-if="result || error" class="space-y-2">
    <!-- Success -->
    <div
      v-if="result"
      class="flex items-start gap-2.5 rounded-lg border-l-2 border-emerald-500 bg-emerald-500/5 px-4 py-3"
    >
      <svg class="mt-0.5 h-4 w-4 shrink-0 text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
        <path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
      </svg>
      <span class="text-sm text-emerald-300">{{ result }}</span>
    </div>

    <!-- Error -->
    <div
      v-if="error"
      class="flex items-start gap-2.5 rounded-lg border-l-2 border-red-500 bg-red-500/5 px-4 py-3"
    >
      <svg class="mt-0.5 h-4 w-4 shrink-0 text-red-400" fill="none" viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
        <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
      </svg>
      <span class="break-all font-mono text-sm text-red-300">{{ error }}</span>
    </div>
  </div>
</template>
