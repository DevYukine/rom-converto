import { beforeEach, describe, expect, it, vi } from "vitest";
import { useUiStore } from "./ui";

const win = vi.hoisted(() => ({
	maximized: false,
	moved: undefined as (() => void) | undefined,
	resized: undefined as (() => void) | undefined,
	setZoom: vi.fn(async () => {}),
	setMinSize: vi.fn(async () => {}),
}));

vi.mock("~/lib/ipc", () => ({ isTauri: true }));
vi.mock("@tauri-apps/api/webview", () => ({
	getCurrentWebview: () => ({ setZoom: win.setZoom }),
}));
vi.mock("@tauri-apps/api/window", async (importOriginal) => ({
	...(await importOriginal<typeof import("@tauri-apps/api/window")>()),
	currentMonitor: async () => null,
	getCurrentWindow: () => ({
		isMaximized: async () => win.maximized,
		setMinSize: win.setMinSize,
		onMoved: async (handler: () => void) => {
			win.moved = handler;
			return () => {};
		},
		onResized: async (handler: () => void) => {
			win.resized = handler;
			return () => {};
		},
		onScaleChanged: async () => () => {},
	}),
}));

// Runs once every pending promise job has settled.
const flush = () => new Promise<void>((resolve) => setImmediate(resolve));

beforeEach(() => {
	vi.useFakeTimers({ toFake: ["setTimeout", "clearTimeout"] });
	vi.stubGlobal("localStorage", { getItem: () => null, setItem: () => {} });
});

describe("window minimum size", () => {
	it("leaves a maximized window alone and re-applies once it is restored", async () => {
		useUiStore();
		await flush();
		expect(win.setMinSize).toHaveBeenCalledOnce();

		// Maximizing moves the window. On Windows a new minimum re-applies the
		// current size, which un-maximizes it.
		win.maximized = true;
		win.moved!();
		await vi.advanceTimersByTimeAsync(250);
		await flush();
		expect(win.setZoom).toHaveBeenCalledTimes(2);
		expect(win.setMinSize).toHaveBeenCalledOnce();

		// A restore that keeps the top-left corner resizes without moving.
		win.maximized = false;
		win.resized!();
		await vi.advanceTimersByTimeAsync(250);
		await flush();
		expect(win.setMinSize).toHaveBeenCalledTimes(2);
	});
});
