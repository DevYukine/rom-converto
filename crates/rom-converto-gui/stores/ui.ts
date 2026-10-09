import { defineStore } from "pinia";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import { currentMonitor, getCurrentWindow, LogicalSize } from "@tauri-apps/api/window";
import { isTauri } from "~/lib/ipc";
import { useNotifyPrefs } from "~/composables/useNotifyPrefs";

type Theme = "system" | "light" | "dark";
type Scale = 0.9 | 1.0 | 1.15 | 1.3 | 1.5 | 2.0;

// Interface scale steps offered in Settings, smallest first.
export const SCALE_STEPS = [0.9, 1.0, 1.15, 1.3, 1.5, 2.0];

const STORAGE_KEY = "rom-converto:ui";

interface Persisted {
	theme: Theme;
	scale: Scale;
	startImmediately: boolean;
	taskbarProgress: boolean;
	defaultOnConflict: string;
	lastConsolePerOp: Record<string, string>;
	recentOutputDirs: string[];
}

const DEFAULTS: Persisted = {
	theme: "system",
	scale: 1.0,
	startImmediately: true,
	taskbarProgress: true,
	defaultOnConflict: "overwrite",
	lastConsolePerOp: {},
	recentOutputDirs: [],
};

const RECENT_DIRS_MAX = 5;

// Current monitor work area in logical pixels; null when it cannot be read.
export async function readWorkArea(): Promise<{ width: number; height: number } | null> {
	if (!isTauri) {
		return typeof window !== "undefined" && window.screen
			? { width: window.screen.availWidth, height: window.screen.availHeight }
			: null;
	}
	try {
		const monitor = await currentMonitor();
		return monitor ? monitor.workArea.size.toLogical(monitor.scaleFactor) : null;
	} catch {
		return null;
	}
}

// A scale fits when its 800x440 logical px window minimum still fits the
// monitor's work area; without a reading everything is assumed to fit.
export function scaleFitsWorkArea(step: number, area: { width: number; height: number } | null): boolean {
	return !area || (area.width >= 800 * step && area.height >= 440 * step);
}

// Clamps a preferred scale to the largest fitting step; the preference comes
// back unchanged when no work area is known.
export function scaleForWorkArea(preferred: number, area: { width: number; height: number } | null): number {
	if (!area) return preferred;
	for (let i = SCALE_STEPS.length - 1; i >= 0; i--) {
		const step = SCALE_STEPS[i]!;
		if (scaleFitsWorkArea(step, area)) {
			return Math.min(preferred, step);
		}
	}
	return SCALE_STEPS[0]!;
}

function recentDirs(value: unknown): string[] {
	if (!Array.isArray(value)) return [];
	const dirs = value.filter((dir): dir is string => typeof dir === "string" && dir.length > 0);
	return [...new Set(dirs)].slice(0, RECENT_DIRS_MAX);
}

function readPersisted(): Persisted {
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (raw) {
			const p = { ...DEFAULTS, ...(JSON.parse(raw) as Partial<Persisted>) };
			p.recentOutputDirs = recentDirs(p.recentOutputDirs);
			return p;
		}
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
	const recentOutputDirs = ref<string[]>(p.recentOutputDirs);

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

	// The scale actually on screen: the saved preference clamped to the
	// largest step that fits the current work area.
	const appliedScale = ref<number>(p.scale);
	// Current monitor work area in logical pixels, refreshed whenever the
	// scale is applied (including on window move and scale-change events).
	const workArea = ref<{ width: number; height: number } | null>(null);

	// The window minimum is in logical pixels, so it grows with the zoom. A
	// saved scale that no longer fits the monitor clamps down to the largest
	// step that does; the preference itself is left untouched.
	async function applyScale() {
		workArea.value = await readWorkArea();
		const applied = scaleForWorkArea(scale.value, workArea.value);
		appliedScale.value = applied;
		if (isTauri) {
			try {
				await getCurrentWebview().setZoom(applied);
				const area = workArea.value;
				const width = area ? Math.min(800 * applied, area.width) : 800 * applied;
				const height = area ? Math.min(440 * applied, area.height) : 440 * applied;
				await getCurrentWindow().setMinSize(new LogicalSize(width, height));
			} catch {
				// Without the zoom permission or a readable monitor the scale may
				// not apply or clamp; the saved preference is unchanged either way.
			}
		} else if (typeof document !== "undefined") {
			document.documentElement.style.setProperty("zoom", String(applied));
		}
	}

	// Moving the window between monitors changes the fit, so re-clamp; the
	// preference restores itself when the window moves back. Debounced because
	// onMoved fires on every drag frame.
	let scaleTimer: ReturnType<typeof setTimeout> | undefined;
	function queueApplyScale() {
		clearTimeout(scaleTimer);
		scaleTimer = setTimeout(() => void applyScale(), 250);
	}
	if (isTauri) {
		getCurrentWindow().onMoved(queueApplyScale).catch(() => {});
		getCurrentWindow().onScaleChanged(queueApplyScale).catch(() => {});
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
					recentOutputDirs: recentOutputDirs.value,
				} satisfies Persisted),
			);
		} catch {
			// localStorage unavailable; preferences just won't persist.
		}
	}

	function setLastConsole(op: string, console: string) {
		lastConsolePerOp.value = { ...lastConsolePerOp.value, [op]: console };
	}

	function rememberOutputDir(dir: string) {
		if (!dir) return;
		recentOutputDirs.value = recentDirs([dir, ...recentOutputDirs.value]);
	}

	watch(resolvedTheme, applyTheme, { immediate: true });
	watch(scale, applyScale, { immediate: true });
	watch(
		[theme, scale, startImmediately, taskbarProgress, defaultOnConflict, lastConsolePerOp, recentOutputDirs],
		persist,
		{ deep: true },
	);

	return {
		theme,
		scale,
		appliedScale,
		workArea,
		startImmediately,
		taskbarProgress,
		defaultOnConflict,
		soundEnabled,
		lastConsolePerOp,
		recentOutputDirs,
		resolvedTheme,
		setLastConsole,
		rememberOutputDir,
	};
});
