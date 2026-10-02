import { useChdCompressStore } from "~/stores/chd-compress";
import { useCsoCompressStore } from "~/stores/cso-compress";
import { useDolCompressStore } from "~/stores/dol-compress";
import { useNxCompressStore } from "~/stores/nx-compress";
import { useRvlCompressStore } from "~/stores/rvl-compress";
import { useWupCompressStore } from "~/stores/wup-compress";
import type { OpStore } from "~/lib/opdefs/types";
import type { PresetFormat } from "~/types";

export interface PresetBinding {
	format: PresetFormat;
	useStore: () => OpStore;
	// config key -> store field
	map: Record<string, string>;
}

// Maps a compress console to its preset table plus the config-key -> store-field
// translation. ctr, cue and xenon have no config table, so a preset applies
// nothing on those pages.
export const PRESET_BINDINGS: Record<string, PresetBinding> = {
	nx: {
		format: "nx",
		useStore: useNxCompressStore,
		map: { level: "level", mode: "mode", block_size_exp: "blockSizeExp", on_conflict: "onConflict", output_dir: "outputDir", report: "reportFile" },
	},
	dol: {
		format: "dol",
		useStore: useDolCompressStore,
		map: { level: "level", chunk_size: "chunkSize", on_conflict: "onConflict", output_dir: "outputDir", report: "reportFile" },
	},
	rvl: {
		format: "rvl",
		useStore: useRvlCompressStore,
		map: { level: "level", chunk_size: "chunkSize", on_conflict: "onConflict", output_dir: "outputDir", report: "reportFile" },
	},
	chd: {
		format: "chd",
		useStore: useChdCompressStore,
		map: {
			hunk_size: "hunkSize",
			codecs: "codecs",
			level: "level",
			on_conflict: "onConflict",
			output_dir: "outputDir",
			report: "reportFile",
		},
	},
	cso: {
		format: "cso",
		useStore: useCsoCompressStore,
		map: { block_size: "blockSize", on_conflict: "onConflict", output_dir: "outputDir", report: "reportFile" },
	},
	wup: {
		format: "wup",
		useStore: useWupCompressStore,
		map: { level: "level", on_conflict: "onConflict" },
	},
};
