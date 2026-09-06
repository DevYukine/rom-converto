import type { ChdLdInfo } from "~/types/info";
import { add, discContentRom, formatBytes, ldClvTime, ldDiscTypeLabel } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

function ldVbiSummary(vbi: NonNullable<ChdLdInfo["vbi"]>): string {
	const parts = [ldDiscTypeLabel(vbi.disc_type)];
	if (vbi.cav_picture_min != null && vbi.cav_picture_max != null) {
		parts.push(`pic ${vbi.cav_picture_min}-${vbi.cav_picture_max}`);
	}
	if (vbi.clv_start_time && vbi.clv_end_time) {
		parts.push(`${ldClvTime(vbi.clv_start_time)}-${ldClvTime(vbi.clv_end_time)}`);
	}
	if (vbi.chapter_min != null && vbi.chapter_max != null) {
		parts.push(`ch ${vbi.chapter_min}-${vbi.chapter_max}`);
	}
	parts.push(`${vbi.white_flag_count} white flags`);
	return parts.join(" · ");
}

export const chd: KindModule<"chd"> = {
	build(info): InspectBuild {
		const container: InspectField[] = [];
		const hashes: InspectField[] = [];
		add(container, "Container", `CHD v${info.version}`);
		add(container, "Compression", info.compressors.join(", ") || "none");
		add(container, "Compressed Size", formatBytes(info.physical_bytes));
		add(container, "Logical Size", formatBytes(info.logical_bytes));
		add(container, "Ratio", `${info.compression_ratio.toFixed(1)}%`);
		add(container, "Hunk", `${formatBytes(info.hunk_bytes)} × ${info.hunk_count}`);
		add(container, "Unit", formatBytes(info.unit_bytes));
		if (info.dvd) add(container, "DVD", `${info.dvd.total_sectors} sectors · ${info.dvd.layer_class}`);
		if (info.hard_disk) {
			const hd = info.hard_disk;
			add(container, "Hard Disk", `${hd.cylinders}/${hd.heads}/${hd.sectors} · ${hd.sector_bytes} B/sector`);
		}
		if (info.ld) {
			add(container, "LD FPS", info.ld.fps);
			add(container, "LD Field Size", `${info.ld.width}x${info.ld.height}`);
			add(container, "LD Interlaced", info.ld.interlaced ? "yes" : "no");
			add(container, "LD Audio", `${info.ld.channels} ch · ${info.ld.sample_rate} Hz`);
			add(container, "LD Frames", info.ld.frame_count);
			if (info.ld.vbi) add(container, "LD VBI", ldVbiSummary(info.ld.vbi));
		}
		add(container, "Metadata", info.metadata_tags.map((t) => t.tag).join(", "));
		add(hashes, "Raw SHA-1", info.raw_sha1);
		add(hashes, "SHA-1", info.sha1);
		add(hashes, "MD5", info.md5);
		add(hashes, "Parent SHA-1", info.parent_sha1);
		add(hashes, "Parent MD5", info.parent_md5);
		return {
			container,
			rom: info.content ? discContentRom(info.content) : [],
			hashes,
			innerTitle: "Tracks",
			innerFiles: info.tracks.map((t) => {
				let detail = `${t.track_type} · ${t.frames} frames`;
				if (t.pregap > 0) {
					detail += ` · pregap ${t.pregap}`;
					if (t.pgtype) detail += ` (${t.pgtype}${t.pgsub ? `/${t.pgsub}` : ""})`;
				}
				if (t.postgap) detail += ` · postgap ${t.postgap}`;
				if (t.subtype) detail += ` · sub ${t.subtype}`;
				return { name: `Track ${t.number}`, detail };
			}),
		};
	},
	title(info) {
		const fallback = info.version_string || `CHD v${info.version}`;
		if (info.content?.kind === "psp") return info.content.title || info.content.title_id || fallback;
		if (info.content?.kind === "psx") return info.content.volume_id || info.content.title_id || fallback;
		return fallback;
	},
	size: (info) => info.physical_bytes,
	console(info) {
		if (info.content?.kind === "psx") return info.content.console;
		if (info.content?.kind === "psp") return "PSP";
		return "CHD";
	},
	format: () => "CHD",
	media(info) {
		if (info.content?.kind === "psx") return info.content.media;
		if (info.content?.kind === "psp") return "UMD";
		if (info.ld) return "LaserDisc";
		if (info.dvd) return "DVD";
		if (info.hard_disk) return "Hard Disk";
		return info.tracks.length ? "CD" : null;
	},
	meta: (info) =>
		info.content
			? [info.content.title_id, info.content.version && `v${info.content.version}`]
			: [info.compressors.join(", ")],
	stats: (info) => [
		...(info.content?.title_id ? [{ label: "Title ID", value: info.content.title_id }] : []),
		{ label: "Ratio", value: `${info.compression_ratio.toFixed(1)}%`, color: "green" as const },
		{ label: "Hunks", value: String(info.hunk_count) },
	],
};
