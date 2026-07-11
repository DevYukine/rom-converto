export interface Toast {
	id: number;
	message: string;
}

let seq = 0;

// State lives in Nuxt's app-level store so every caller shares one queue,
// regardless of whether the composable is auto-imported or imported by path
// (a plain module-level ref can duplicate across those two resolutions).
export function useToast() {
	const toasts = useState<Toast[]>("toasts", () => []);

	function show(message: string, ms = 1500) {
		const id = ++seq;
		toasts.value.push({ id, message });
		setTimeout(() => {
			const i = toasts.value.findIndex((t) => t.id === id);
			if (i >= 0) toasts.value.splice(i, 1);
		}, ms);
	}

	return { toasts, show };
}
