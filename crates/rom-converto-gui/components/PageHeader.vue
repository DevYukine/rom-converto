<script setup lang="ts">
defineProps<{
  title: string;
  description: string;
  loading?: boolean;
  hasResult?: boolean;
  hasError?: boolean;
}>();
</script>

<template>
  <div class="mb-6 flex items-start justify-between">
    <div>
      <h2 class="text-xl font-semibold text-zinc-100">{{ title }}</h2>
      <p class="mt-1 text-sm text-zinc-400">{{ description }}</p>
    </div>
    <span
      v-if="loading"
      class="mt-1 inline-flex items-center gap-1.5 rounded-full bg-sky-500/10 px-2.5 py-1 text-xs font-medium text-sky-400"
    >
      <span class="h-1.5 w-1.5 animate-pulse rounded-full bg-sky-400" />
      Running
    </span>
    <span
      v-else-if="hasError"
      class="mt-1 inline-flex items-center gap-1.5 rounded-full bg-red-500/10 px-2.5 py-1 text-xs font-medium text-red-400"
    >
      <span class="h-1.5 w-1.5 rounded-full bg-red-400" />
      Failed
    </span>
    <span
      v-else-if="hasResult"
      class="mt-1 inline-flex items-center gap-1.5 rounded-full bg-emerald-500/10 px-2.5 py-1 text-xs font-medium text-emerald-400"
    >
      <span class="h-1.5 w-1.5 rounded-full bg-emerald-400" />
      Done
    </span>
  </div>
</template>
