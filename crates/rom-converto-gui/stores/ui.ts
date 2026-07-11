import { defineStore } from "pinia";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { isTauri } from "~/lib/ipc";
import { useNotifyPrefs } from "~/composables/useNotifyPrefs";

type Theme = "system" | "light" | "dark";
type Scale = 0.9 | 1.0 | 1.15 | 1.3;

const STORAGE_KEY = "rom-converto:ui";

interface Persisted {
	theme: Theme;
	scale: Scale;
	startImmediately: boolean;
	taskbarProgress: boolean;
	defaultOnConflict: string;
	lastConsolePerOp: Record<string, string>;
}

const DEFAULTS: Persisted = {
	theme: "system",
	scale: 1.0,
	startImmediately: true,
	taskbarProgress: true,
	defaultOnConflict: "overwrite",
	lastConsolePerOp: {},
};

function readPersisted(): Persisted {
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (raw) return { ...DEFAULTS, ...(JSON.parse(raw) as Partial<Persisted>) };
	} catch {
		// localStorage unavailable or corrupt; fall through to defaults.
	}
	return { ...DEFAULTS };
}

export const useUiStore = defineStore("ui", () => {
	const p = readPersisted();
	const theme = ref<Theme>(p.theme);
	const scale = ref<Scale>(p.scale);
	const startImmediately = ref(p.startImmediately);
	const taskbarProgress = ref(p.taskbarProgress);
	const defaultOnConflict = ref(p.defaultOnConflict);
	const lastConsolePerOp = ref<Record<string, string>>(p.lastConsolePerOp);

	// Completion sound persists under its own key via the shared composable.
	const { soundEnabled } = useNotifyPrefs();

	const systemLight = ref(false);
	if (typeof window !== "undefined" && window.matchMedia) {
		const media = window.matchMedia("(prefers-color-scheme: light)");
		systemLight.value = media.matches;
		media.addEventListener("change", (e) => {
			systemLight.value = e.matches;
		});
	}

	const resolvedTheme = computed<"light" | "dark">(() =>
		theme.value === "system" ? (systemLight.value ? "light" : "dark") : theme.value,
	);

	function applyTheme() {
		if (typeof document === "undefined") return;
		const root = document.documentElement;
		root.dataset.theme = resolvedTheme.value;
		root.style.colorScheme = resolvedTheme.value;
	}

	async function applyScale() {
		if (isTauri) {
			try {
				await getCurrentWebview().setZoom(scale.value);
			} catch {
				// Zoom permission missing or webview unavailable; leave scale unchanged.
			}
		} else if (typeof document !== "undefined") {
			document.documentElement.style.setProperty("zoom", String(scale.value));
		}
	}

	function persist() {
		try {
			localStorage.setItem(
				STORAGE_KEY,
				JSON.stringify({
					theme: theme.value,
					scale: scale.value,
					startImmediately: startImmediately.value,
					taskbarProgress: taskbarProgress.value,
					defaultOnConflict: defaultOnConflict.value,
					lastConsolePerOp: lastConsolePerOp.value,
				} satisfies Persisted),
			);
		} catch {
			// localStorage unavailable; preferences just won't persist.
		}
	}

	function setLastConsole(op: string, console: string) {
		lastConsolePerOp.value = { ...lastConsolePerOp.value, [op]: console };
	}

	watch(resolvedTheme, applyTheme, { immediate: true });
	watch(scale, applyScale, { immediate: true });
	watch(
		[theme, scale, startImmediately, taskbarProgress, defaultOnConflict, lastConsolePerOp],
		persist,
		{ deep: true },
	);

	return {
		theme,
		scale,
		startImmediately,
		taskbarProgress,
		defaultOnConflict,
		soundEnabled,
		lastConsolePerOp,
		resolvedTheme,
		setLastConsole,
	};
});
