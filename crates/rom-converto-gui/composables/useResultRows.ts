import { computed, type Ref } from "vue";
import { basename } from "~/composables/useDerivedPath";

// Shared status-chip filtering for the DAT scan and verify result lists: both
// count rows by a status field, show only the selected status, and toggle the
// active chip off on a second click.
export function useResultRows<T, S extends string>(
  rows: Ref<T[]>,
  statusOf: (row: T) => S,
  filter: Ref<S | "all">,
) {
  const counts = computed<Record<string, number>>(() => {
    const c: Record<string, number> = {};
    for (const r of rows.value) c[statusOf(r)] = (c[statusOf(r)] ?? 0) + 1;
    return c;
  });

  const visibleRows = computed(() =>
    rows.value.filter((r) => filter.value === "all" || statusOf(r) === filter.value),
  );

  function toggleFilter(status: S) {
    filter.value = filter.value === status ? "all" : status;
  }

  return { counts, visibleRows, toggleFilter };
}

// Right-click menu for one result row: the path, the detail line when the row
// has one, and both joined as the row text.
export function rowContextItems(path: string, detail?: string) {
  const items = [{ label: "Copy file path", value: path }];
  if (detail) items.push({ label: "Copy details", value: detail });
  items.push({ label: "Copy row", value: [basename(path), detail].filter(Boolean).join(" · ") });
  return items;
}
