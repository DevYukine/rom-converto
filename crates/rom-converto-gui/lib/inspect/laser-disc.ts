import { add, formatBytes, ldClvTime, ldDiscTypeLabel } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

export const laserDisc: KindModule<"laser_disc"> = {
	build(info): InspectBuild {
		const container: InspectField[] = [];
		add(container, "Format", `LaserDisc AVI (${info.video_fourcc})`);
		add(container, "Resolution", `${info.video_width}x${info.video_height}`);
		add(container, "FPS", info.fps.toFixed(3));
		add(container, "Duration", `${info.duration_seconds.toFixed(1)}s`);
		add(container, "Frame Count", info.frame_count);
		add(container, "Audio", `${info.audio_channels} ch · ${info.audio_rate} Hz · ${info.audio_bits}-bit`);
		add(container, "Size", formatBytes(info.file_size_bytes));
		add(container, "Interlaced", info.interlaced ? "yes" : "no");
		add(container, "Hunk Bytes", formatBytes(info.bytes_per_frame));
		add(container, "Fields", info.fields);
		if (info.vbi) {
			const vbi = info.vbi;
			add(container, "Disc Type", ldDiscTypeLabel(vbi.disc_type));
			if (vbi.cav_picture_min != null && vbi.cav_picture_max != null) {
				add(container, "Picture Range", `${vbi.cav_picture_min}-${vbi.cav_picture_max}`);
			}
			if (vbi.clv_start && vbi.clv_end) {
				add(container, "Time Range", `${ldClvTime(vbi.clv_start)}-${ldClvTime(vbi.clv_end)}`);
			}
			if (vbi.chapter_min != null && vbi.chapter_max != null) {
				add(container, "Chapters", `${vbi.chapter_min}-${vbi.chapter_max}`);
			}
			add(container, "White Flags", vbi.white_flag_count);
			add(container, "Lead-in / Lead-out", `${vbi.lead_in ? "yes" : "no"} / ${vbi.lead_out ? "yes" : "no"}`);
			add(container, "Fields Without Code", vbi.fields_without_code);
		}
		return { container };
	},
	title: () => "LaserDisc rip",
	size: (info) => info.file_size_bytes,
	console: () => "LASERDISC",
	format: () => "AVI",
	media: () => "LaserDisc",
	meta: (info) => [`${info.video_width}x${info.video_height}`, `${info.fps.toFixed(2)} fps`],
};
