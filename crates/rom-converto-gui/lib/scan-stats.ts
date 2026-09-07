function normalize(p: string): string {
	return p.replace(/\\/g, "/").replace(/\/+$/, "");
}

// Path shown for a scanned file: relative to the scan root, so folders full of
// identically named files (content/tmd style layouts) stay distinguishable.
export function relativePath(path: string, root: string): string {
	const p = normalize(path);
	const r = normalize(root);
	if (r && p.toLowerCase().startsWith(`${r.toLowerCase()}/`)) return p.slice(r.length + 1);
	return p.slice(p.lastIndexOf("/") + 1);
}

export function formatRate(perSecond: number): string {
	if (perSecond >= 10) return `${Math.round(perSecond)} files/s`;
	if (perSecond >= 1) return `${perSecond.toFixed(1)} files/s`;
	if (perSecond > 0) return `${Math.round(1 / perSecond)} s/file`;
	return "";
}

// Coarse and padded upward: a bar that finishes early is forgiven, one that
// overruns its own estimate is not.
export function formatEta(seconds: number): string {
	if (!Number.isFinite(seconds) || seconds < 0) return "";
	if (seconds < 10) return "a few seconds left";
	if (seconds < 60) return `about ${Math.ceil(seconds / 10) * 10} s left`;
	const minutes = Math.ceil(seconds / 60);
	if (minutes < 60) return `about ${minutes} min left`;
	const h = Math.floor(minutes / 60);
	const m = minutes % 60;
	return m ? `about ${h} h ${m} min left` : `about ${h} h left`;
}

export function formatElapsed(ms: number): string {
	const s = Math.round(ms / 1000);
	if (s < 60) return `${s} s`;
	const m = Math.floor(s / 60);
	const rest = s % 60;
	if (m < 60) return rest ? `${m} min ${rest} s` : `${m} min`;
	const h = Math.floor(m / 60);
	return m % 60 ? `${h} h ${m % 60} min` : `${h} h`;
}

// Exponentially smoothed units-per-second meter. Samples closer together than
// half a second are folded into the next one so bursts of tiny files do not
// swing the estimate; a counter that goes backwards means a new phase started.
const RATE_ALPHA = 0.3;
const RATE_MIN_INTERVAL_MS = 500;

export function createRateMeter() {
	let last: { t: number; n: number } | null = null;
	let rate = 0;
	return {
		reset() {
			last = null;
			rate = 0;
		},
		sample(t: number, n: number): number {
			if (!last || n < last.n) {
				last = { t, n };
				rate = 0;
				return rate;
			}
			const dt = t - last.t;
			if (dt < RATE_MIN_INTERVAL_MS) return rate;
			const inst = ((n - last.n) * 1000) / dt;
			rate = rate > 0 ? rate + RATE_ALPHA * (inst - rate) : inst;
			last = { t, n };
			return rate;
		},
	};
}
