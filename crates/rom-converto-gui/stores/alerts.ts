import { defineStore } from "pinia";

export type AlertType = "error" | "ok" | "plain";

export interface AlertAction {
	label: string;
	run?: () => void;
}

export interface Alert {
	id: string;
	type: AlertType;
	title: string;
	body: string;
	actions?: AlertAction[];
	meta: string;
	ts: number;
	unread: boolean;
}

export const useAlertsStore = defineStore("alerts", () => {
	const alerts = ref<Alert[]>([]);

	const unreadCount = computed(() => alerts.value.filter((a) => a.unread).length);

	function push(a: Omit<Alert, "id" | "ts" | "unread"> & { unread?: boolean }): string {
		const id = crypto.randomUUID();
		alerts.value = [{ ...a, id, ts: Date.now(), unread: a.unread ?? true }, ...alerts.value];
		return id;
	}

	function markRead(id: string) {
		alerts.value = alerts.value.map((a) => (a.id === id && a.unread ? { ...a, unread: false } : a));
	}

	function markAllRead() {
		alerts.value = alerts.value.map((a) => (a.unread ? { ...a, unread: false } : a));
	}

	function clear() {
		alerts.value = [];
	}

	function remove(id: string) {
		alerts.value = alerts.value.filter((a) => a.id !== id);
	}

	return { alerts, unreadCount, push, markRead, markAllRead, clear, remove };
});
