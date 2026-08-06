import { ref } from "vue";

export interface ContextMenuItem {
	label: string;
	value: string;
}

const open = ref(false);
const x = ref(0);
const y = ref(0);
const items = ref<ContextMenuItem[]>([]);

export function useContextMenu() {
	return { open, x, y, items };
}

export function openContextMenu(e: MouseEvent, menuItems: ContextMenuItem[]) {
	e.preventDefault();
	x.value = e.clientX;
	y.value = e.clientY;
	items.value = menuItems;
	open.value = true;
}

export function closeContextMenu() {
	open.value = false;
}
