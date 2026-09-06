import type { XboxInfo } from "~/types/info";
import { add, formatBytes, hex } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

function formatXboxPartitionKind(pk: XboxInfo["partition_kind"]): string {
	if (typeof pk === "object") return `X360 Extra (+${pk.x360_extra})`;
	switch (pk) {
		case "trimmed":
			return "Trimmed";
		case "xgd1":
			return "XGD1";
		case "xgd2":
			return "XGD2";
		case "xgd3":
			return "XGD3";
	}
}

export const xbox: KindModule<"xbox"> = {
	build(info): InspectBuild {
		const container: InspectField[] = [];
		const rom: InspectField[] = [];
		add(container, "Container", "XISO");
		add(container, "Partition", formatXboxPartitionKind(info.partition_kind));
		add(container, "Logical Size", formatBytes(info.image_size));
		add(container, "Files", `${info.file_count} (${info.dir_count} dirs)`);
		add(container, "File Data", formatBytes(info.total_file_bytes));
		add(container, "Root", `sector ${info.root_sector} · ${formatBytes(info.root_size)}`);
		add(container, "Base", `0x${info.base.toString(16).toUpperCase()}`);
		const xbe = info.xbe;
		const xex = info.xex;
		add(rom, "Title", xbe?.title_name || xex?.title_name);
		add(rom, "Title ID", xbe ? `${xbe.title_id_hex} (${xbe.title_id_code})` : xex?.title_id_hex);
		add(rom, "Content Type", "Game");
		add(rom, "Version", xbe?.version ?? xex?.version);
		add(rom, "Region", (xbe ?? xex)?.region_names.join(", "));
		add(rom, "Size", formatBytes(info.image_size));
		add(rom, "Media ID", xex && xex.media_id.toString(16).padStart(8, "0").toUpperCase());
		add(rom, "Disc", xbe ? xbe.disc_number : xex && `${xex.disc_number}/${xex.disc_count}`);
		add(rom, "Allowed Media", xbe?.allowed_media_names.join(", "));
		add(rom, "Original PE Name", xex?.original_pe_name);
		if (xbe) {
			add(rom, "Ratings", `0x${hex(xbe.ratings, 8)}`);
			if (xbe.cert_timestamp > 0) {
				add(rom, "Cert Timestamp", new Date(xbe.cert_timestamp * 1000).toISOString().slice(0, 10));
			}
			add(rom, "Alternate Title IDs", xbe.alternate_title_ids.map((id) => hex(id, 8)).join(", "));
		}
		if (xex && xex.platform > 0) add(rom, "Platform", xex.platform);
		add(rom, "Base Version", xex?.base_version);
		const innerFiles = info.root_entries.map((e) => ({ name: e.name, detail: e.is_dir ? "dir" : formatBytes(e.size) }));
		if (info.file_count + info.dir_count > info.root_entries.length) {
			innerFiles.push({ name: `${info.file_count} files, ${info.dir_count} dirs`, detail: "" });
		}
		return { container, rom, innerTitle: "Disc Files", innerFiles };
	},
	title: (info) =>
		info.xbe?.title_name || info.xex?.title_name || `${formatXboxPartitionKind(info.partition_kind)} image`,
	size: (info) => info.image_size,
	console: () => "XBOX",
	format: () => "DISC",
	media: () => "DVD",
	stats(info) {
		const titleIdHex = info.xbe?.title_id_hex ?? info.xex?.title_id_hex;
		return [
			...(titleIdHex ? [{ label: "Title ID", value: titleIdHex }] : []),
			{ label: "Partition", value: formatXboxPartitionKind(info.partition_kind) },
			{ label: "Files", value: String(info.file_count) },
		];
	},
	titleId: (info) => (info.xbe || info.xex ? (info.xbe?.title_id_hex ?? info.xex?.title_id_hex ?? "") : null),
};
