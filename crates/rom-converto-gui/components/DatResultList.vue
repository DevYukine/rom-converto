<script setup lang="ts">
export interface DatResultRow {
  key: string;
  icon: "ok" | "warn" | "info" | "error";
  primary: string;
  secondary?: string;
  badge?: string;
  badgeColor?: "emerald" | "amber" | "sky" | "red" | "zinc";
}

defineProps<{
  rows: DatResultRow[];
}>();

const badgeClasses: Record<NonNullable<DatResultRow["badgeColor"]>, string> = {
  emerald: "bg-emerald-500/20 text-emerald-300",
  amber: "bg-amber-500/20 text-amber-300",
  sky: "bg-sky-500/20 text-sky-300",
  red: "bg-red-500/20 text-red-300",
  zinc: "bg-zinc-700/50 text-zinc-400",
};
</script>

<template>
  <div class="max-h-60 space-y-1 overflow-y-auto rounded-lg border border-zinc-700/50 bg-zinc-900/50 p-2 xl:max-h-80">
    <div
      v-for="row in rows"
      :key="row.key"
      class="flex items-center gap-2 rounded-md px-2.5 py-1.5 text-sm"
    >
      <span class="flex h-4 w-4 shrink-0 items-center justify-center">
        <svg
          v-if="row.icon === 'ok'"
          class="h-4 w-4 text-emerald-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="2"
          aria-hidden="true"
        >
          <path stroke-linecap="round" stroke-linejoin="round" d="M5 13l4 4L19 7" />
        </svg>
        <svg
          v-else-if="row.icon === 'warn'"
          class="h-4 w-4 text-amber-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="2"
          aria-hidden="true"
        >
          <path stroke-linecap="round" stroke-linejoin="round" d="M12 9v3.75m-9.303 3.376c-.866 1.5.217 3.374 1.948 3.374h14.71c1.73 0 2.813-1.874 1.948-3.374L13.949 3.378c-.866-1.5-3.032-1.5-3.898 0L2.697 16.126zM12 15.75h.007v.008H12v-.008z" />
        </svg>
        <svg
          v-else-if="row.icon === 'error'"
          class="h-4 w-4 text-red-400"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="2"
          aria-hidden="true"
        >
          <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
        </svg>
        <svg
          v-else
          class="h-4 w-4 text-zinc-500"
          fill="none"
          viewBox="0 0 24 24"
          stroke="currentColor"
          stroke-width="2"
          aria-hidden="true"
        >
          <circle cx="12" cy="12" r="9" />
          <path stroke-linecap="round" d="M12 8v4.5m0 3h.008" />
        </svg>
      </span>

      <div class="min-w-0 flex-1">
        <div class="truncate text-zinc-200" :title="row.primary">{{ row.primary }}</div>
        <div v-if="row.secondary" class="truncate text-xs text-zinc-500" :title="row.secondary">
          {{ row.secondary }}
        </div>
      </div>

      <span
        v-if="row.badge"
        class="shrink-0 rounded bg-zinc-700/50 px-1.5 py-0.5 text-[10px] font-medium text-zinc-400"
        :class="row.badgeColor ? badgeClasses[row.badgeColor] : undefined"
      >
        {{ row.badge }}
      </span>
    </div>

    <div v-if="rows.length === 0" class="px-2.5 py-3 text-center text-sm text-zinc-500">
      No results
    </div>
  </div>
</template>
