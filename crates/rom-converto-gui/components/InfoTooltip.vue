<script setup lang="ts">
withDefaults(
  defineProps<{
    message: string;
    placement?: "top" | "bottom";
    block?: boolean;
  }>(),
  { placement: "top" },
);

const tipId = useId();
const open = ref(false);
</script>

<template>
  <span
    class="relative"
    :class="block ? 'flex w-full' : 'inline-flex'"
    tabindex="0"
    :title="message"
    :aria-describedby="tipId"
    @mouseenter="open = true"
    @mouseleave="open = false"
    @focusin="open = true"
    @focusout="open = false"
  >
    <slot />
    <span
      :id="tipId"
      role="tooltip"
      class="pointer-events-none absolute left-1/2 z-50 w-max max-w-xs -translate-x-1/2 rounded-md border border-zinc-700 bg-zinc-800 px-2 py-1 text-xs text-zinc-200 shadow-lg transition-opacity duration-100"
      :class="[
        placement === 'top' ? 'bottom-full mb-1' : 'top-full mt-1',
        open ? 'opacity-100' : 'opacity-0',
      ]"
    >
      {{ message }}
    </span>
  </span>
</template>
