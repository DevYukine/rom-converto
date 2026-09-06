// Op stores are plain field bags: stores/queue.ts owns execution, so they only
// hold the form state OpPage binds to plus a $reset back to the defaults.
// `defaults` is re-evaluated on every reset so values sourced from other
// stores (e.g. ui.defaultOnConflict) pick up the current setting.

import { defineStore } from "pinia";
import type { Ref } from "vue";

export function makeOpStore<T extends Record<string, unknown>>(id: string, defaults: () => T) {
  return defineStore(id, () => {
    const fields = Object.fromEntries(
      Object.entries(defaults()).map(([key, value]) => [key, ref(value)]),
    ) as { [K in keyof T]: Ref<T[K]> };

    function $reset() {
      const next = defaults();
      for (const key in next) fields[key].value = next[key];
    }

    return { ...fields, $reset };
  });
}
