import { ageRatingDisplayName, contentTypeDisplayName, languageDisplayName } from "../display";
import { add, englishFirst, formatBytes, hex } from "./shared";
import type { InspectBuild, InnerFile, InspectField, KindModule } from "./types";

// "disc"/"nus" sources ship encrypted; "loadiine"/"wua" are already decrypted extractions.
export function wupEncryption(sourceKind: string): string | null {
	if (sourceKind.startsWith("disc") || sourceKind.startsWith("nus")) return "encrypted";
	if (sourceKind.startsWith("loadiine") || sourceKind.startsWith("wua")) return "decrypted";
	return null;
}

export const wup: KindModule<"wup"> = {
	build(info): InspectBuild {
		const rom: InspectField[] = [];
		let innerTitle: string;
		let innerFiles: InnerFile[];
		const meta = info.meta;
		add(rom, "Title", englishFirst(meta?.long_names.entries, (e) => e[0])?.[1] || info.title_id_hex);
		add(rom, "Title ID", info.title_id_hex);
		add(rom, "Content Type", info.content_kind ? contentTypeDisplayName(info.content_kind) : info.title_type);
		add(rom, "Encryption", wupEncryption(info.source_kind));
		add(
			rom,
			"Version",
			info.update_version != null ? `v${info.update_version} (base v${info.title_version})` : `v${info.title_version}`,
		);
		if (meta) {
			add(rom, "Region", meta.region_names.join(", "));
			add(rom, "Languages", meta.long_names.entries.map((e) => languageDisplayName(e[0])).join(", "));
			add(
				rom,
				"Publisher",
				englishFirst(meta.publishers.entries, (e) => e[0])?.[1] || meta.company_name || meta.company_code,
			);
			add(
				rom,
				"Age Ratings",
				Object.entries(meta.age_ratings)
					.sort(([a], [b]) => a.localeCompare(b))
					.map(([org, age]) => `${ageRatingDisplayName(org)} ${age}+`)
					.join(", "),
			);
		}
		add(rom, "Size", formatBytes(info.total_content_size));
		if (meta) add(rom, "Product Code", meta.product_code);
		if (info.content_count > 0) add(rom, "Contents", String(info.content_count));
		add(rom, "OS Version", info.os_version);
		if (meta) {
			add(rom, "Mastered", meta.mastering_date);
			if (meta.save_size) add(rom, "Save Size", formatBytes(meta.save_size));
		}
		add(rom, "SDK Version", info.sdk_version);
		add(rom, "Access Rights", `0x${hex(info.access_rights, 8)}`);
		add(rom, "Group ID", `0x${hex(info.group_id, 4)}`);
		if (meta) {
			if (meta.app_size) add(rom, "App Size", formatBytes(meta.app_size));
			add(
				rom,
				"Boss Storage",
				[meta.boss_size, meta.common_boss_size, meta.account_boss_size]
					.filter((n): n is number => !!n)
					.map((n) => formatBytes(n))
					.join(" · "),
			);
			if (meta.eula_version != null) add(rom, "EULA", `v${meta.eula_version}`);
			if (meta.drc_use != null) add(rom, "GamePad", meta.drc_use ? "yes" : "no");
			if (meta.e_manual != null) add(rom, "e-Manual", meta.e_manual ? "yes" : "no");
			if (meta.network_use != null || meta.online_account_use != null) {
				add(
					rom,
					"Network",
					[
						meta.network_use != null ? `use ${meta.network_use}` : "",
						meta.online_account_use != null ? `account ${meta.online_account_use}` : "",
					]
						.filter(Boolean)
						.join(" · "),
				);
			}
		}
		if (info.disc_partitions.length) {
			innerTitle = "Disc Partitions";
			innerFiles = info.disc_partitions.map((p) => ({
				name: p.name,
				detail: `${p.kind} · sector ${p.start_sector}`,
			}));
		} else {
			innerTitle = "Bundled Titles";
			innerFiles = info.bundled_titles.map((b) => ({
				name: b.title_type,
				detail: `${b.title_id_hex} · v${b.title_version}`,
			}));
		}
		return { rom, innerTitle, innerFiles };
	},
	title: (info) => englishFirst(info.meta?.long_names?.entries, (e) => e[0])?.[1] || info.title_id_hex,
	size: (info) => info.total_content_size,
	console: () => "WII U",
	format: (info) => info.source_kind.toUpperCase(),
	meta: (info) => [
		englishFirst(info.meta?.publishers?.entries, (e) => e[0])?.[1],
		info.meta?.region_names?.join(", "),
	],
	stats: (info) => [
		{ label: "Title ID", value: info.title_id_hex },
		{ label: "Contents", value: String(info.content_count) },
	],
	titleId: (info) => info.title_id_hex,
};
