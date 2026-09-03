import { defineStore } from "pinia";
import { computed, ref, watch } from "vue";
import { isTauri } from "~/lib/ipc";
import { CHECK_DELAY_MS, CHECK_INTERVAL_MS, createUpdater, promptOpen, type UpdateState } from "~/lib/updater";
import { useQueueStore } from "~/stores/queue";

const STORAGE_KEY = "rom-converto:updates";

interface Persisted {
	autoCheck: boolean;
	skippedVersion: string;
}

function readPersisted(): Persisted {
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (raw) return { autoCheck: true, skippedVersion: "", ...(JSON.parse(raw) as Partial<Persisted>) };
	} catch {
		// localStorage unavailable or corrupt; fall through to defaults.
	}
	return { autoCheck: true, skippedVersion: "" };
}

export const useUpdatesStore = defineStore("updates", () => {
	const queue = useQueueStore();
	const p = readPersisted();
	const autoCheck = ref(p.autoCheck);
	const skippedVersion = ref(p.skippedVersion);
	// "Later" only silences the version until the next launch.
	const dismissedVersion = ref("");
	const installStarted = ref(false);

	const state = ref<UpdateState>({ phase: "current", availableVersion: "", progress: -1, error: "" });
	const updater = createUpdater(isTauri, (next) => (state.value = next));

	const open = computed(() =>
		promptOpen(state.value, [skippedVersion.value, dismissedVersion.value], installStarted.value),
	);
	// Installing relaunches the app, which would kill running conversions.
	const blocked = computed(() => queue.running.length > 0);

	let delay: ReturnType<typeof setTimeout> | undefined;
	let interval: ReturnType<typeof setInterval> | undefined;

	function check() {
		installStarted.value = false;
		return updater.checkForUpdate();
	}

	async function install() {
		if (blocked.value) return;
		installStarted.value = true;
		await updater.installUpdate();
	}

	async function retry() {
		// Keep the toast up if this check fails too, so the outcome is visible.
		await updater.checkForUpdate();
		if (state.value.phase === "available") await install();
	}

	function later() {
		dismissedVersion.value = state.value.availableVersion;
		installStarted.value = false;
	}

	function skip() {
		skippedVersion.value = state.value.availableVersion;
		installStarted.value = false;
	}

	function schedule() {
		clearTimeout(delay);
		clearInterval(interval);
		if (!isTauri || !autoCheck.value) return;
		delay = setTimeout(() => {
			void check();
			interval = setInterval(() => void check(), CHECK_INTERVAL_MS);
		}, CHECK_DELAY_MS);
	}

	watch(autoCheck, schedule);
	watch([autoCheck, skippedVersion], () => {
		try {
			localStorage.setItem(
				STORAGE_KEY,
				JSON.stringify({ autoCheck: autoCheck.value, skippedVersion: skippedVersion.value } satisfies Persisted),
			);
		} catch {
			// localStorage unavailable; preferences just won't persist.
		}
	});

	return { state, autoCheck, open, blocked, check, install, retry, later, skip, start: schedule };
});
