import { ageRatingDisplayName, contentTypeDisplayName, languageDisplayName } from "../display";
import { add, englishFirst, formatBytes, formatMaker, hex } from "./shared";
import type { InspectBuild, InnerFile, InspectField, KindModule } from "./types";

export const ctr: KindModule<"ctr"> = {
	build(info): InspectBuild {
		const container: InspectField[] = [];
		const rom: InspectField[] = [];
		let innerTitle: string | undefined;
		let innerFiles: InnerFile[] | undefined;
		if (info.compressed) {
			add(container, "Container", "Z3DS");
			add(container, "Compression", "zstd");
			add(container, "Compressed Size", formatBytes(info.physical_bytes));
		}
		const smdh = info.smdh;
		const title = englishFirst(smdh?.titles, (t) => t.language);
		add(rom, "Title", title?.long_description || info.product_code || info.title_id);
		add(rom, "Title ID", info.title_id);
		add(rom, "Content Type", info.content_kind ? contentTypeDisplayName(info.content_kind) : info.format.toUpperCase());
		if (smdh) {
			add(rom, "Region", smdh.region_names.join(", "));
			add(rom, "Languages", smdh.titles.map((t) => languageDisplayName(t.language)).join(", "));
		}
		add(rom, "Publisher", title?.publisher || formatMaker(info.maker_code, info.maker_name));
		if (smdh) {
			add(
				rom,
				"Age Ratings",
				smdh.age_ratings
					.map(
						(r) =>
							`${ageRatingDisplayName(r.region)} ${r.age}+${r.pending ? " (pending)" : ""}${r.banned ? " (banned)" : ""}`,
					)
					.join(", "),
			);
		}
		add(rom, "Size", formatBytes(info.physical_bytes));
		add(rom, "Program ID", info.program_id);
		add(rom, "Product Code", info.product_code);
		add(rom, "Maker", formatMaker(info.maker_code, info.maker_name));
		if (info.cartridge_size) add(rom, "Cartridge", formatBytes(info.cartridge_size));
		add(rom, "Encryption", info.ncch_encrypted ? "encrypted" : "decrypted");
		if (smdh) {
			add(rom, "EULA", `v${smdh.eula_version_major}.${smdh.eula_version_minor}`);
			add(rom, "Flags", `0x${hex(smdh.flags, 8)}`);
		}
		if (info.ncsd_partitions.length) {
			innerTitle = "Partitions";
			innerFiles = info.ncsd_partitions.map((p) => ({
				name: p.name,
				detail: `${formatBytes(p.size)} · 0x${p.offset.toString(16).toUpperCase()}`,
			}));
		} else if (info.cia_contents.length) {
			innerTitle = "Contents";
			innerFiles = info.cia_contents.map((c) => ({
				name: `Content ${c.index}`,
				detail: `${c.content_id} · ${formatBytes(c.size)}${c.encrypted ? " · encrypted" : ""}`,
			}));
		}
		return { container, rom, innerTitle, innerFiles };
	},
	title: (info) =>
		englishFirst(info.smdh?.titles, (t) => t.language)?.long_description || info.product_code || info.title_id,
	size: (info) => info.physical_bytes,
	console: () => "3DS",
	format: (info) => info.format.toUpperCase(),
	meta: (info) => [formatMaker(info.maker_code, info.maker_name), info.smdh?.region_names.join(", ")],
	stats: (info) => [
		{ label: "Title ID", value: info.title_id },
		{ label: "Encryption", value: info.ncch_encrypted ? "encrypted" : "decrypted ✓" },
		...(info.compressed ? [{ label: "Compressed", value: "zstd" }] : []),
	],
	titleId: (info) => info.title_id,
};
