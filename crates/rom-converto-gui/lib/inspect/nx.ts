import { ageRatingDisplayName, contentTypeDisplayName, enumDisplayName, languageDisplayName } from "../display";
import { add, englishFirst, formatBytes } from "./shared";
import type { InspectBuild, InspectField, KindModule } from "./types";

export const nx: KindModule<"nx"> = {
	build(info): InspectBuild {
		const container: InspectField[] = [];
		const rom: InspectField[] = [];
		add(container, "Container", info.container_kind.toUpperCase());
		add(container, "Compression", info.is_compressed ? "zstd" : "none");
		add(container, "Compressed Size", formatBytes(info.physical_bytes));
		add(container, "Distribution", enumDisplayName(info.distribution));
		add(container, "Structure", enumDisplayName(info.structure));
		add(container, "NCAs", `${info.nca_names.length} (${info.cnmt_nca_names.length} meta)`);
		add(container, "Tickets", info.tickets.length);
		if (info.xci_partitions?.length) {
			add(
				container,
				"XCI Partitions",
				info.xci_partitions.map((p) => `${p.name} (${p.file_count} files, ${formatBytes(p.total_size)})`).join("; "),
			);
		}
		const full = info.full;
		const ctrl = full?.control;
		const title = englishFirst(ctrl?.titles, (t) => t.language);
		add(rom, "Title", title?.name || info.container_kind.toUpperCase());
		if (full) {
			add(rom, "Title ID", full.application_title_id_hex);
			add(rom, "Content Type", contentTypeDisplayName(full.title_kind));
			add(
				rom,
				"Version",
				ctrl?.display_version ? `${ctrl.display_version} (v${full.title_version})` : `v${full.title_version}`,
			);
		}
		if (ctrl) add(rom, "Languages", ctrl.supported_languages.map((l) => languageDisplayName(l)).join(", "));
		add(rom, "Publisher", title?.publisher);
		if (ctrl) {
			add(rom, "Age Ratings", ctrl.age_ratings.map((r) => `${ageRatingDisplayName(r.organization)} ${r.age}+`).join(", "));
		}
		add(rom, "Size", formatBytes(full?.total_content_size ?? info.physical_bytes));
		add(rom, "Base Title", full?.base_application_id_hex);
		add(rom, "Contents", full?.content_count);
		// NCZ crypto sections are stored decrypted; the AES-CTR is re-applied on read.
		add(
			rom,
			"Encryption",
			info.container_kind === "nsz" || info.container_kind === "xcz"
				? "decrypted (ncz sections)"
				: info.tickets.length
					? "encrypted (titlekey)"
					: "encrypted (standard keys)",
		);
		if (full) {
			const req = full.required_system_version;
			if (req > 0) add(rom, "Required System", `${(req >> 26) & 0x3f}.${(req >> 20) & 0x3f}.${(req >> 16) & 0xf}`);
			add(rom, "Storage ID", full.storage_id);
		}
		if (ctrl) {
			add(rom, "Attributes", ctrl.attributes.join(", "));
			add(rom, "Startup Account", ctrl.startup_user_account_name);
			add(rom, "Screenshot", ctrl.screenshot === 0 ? "Allowed" : "Blocked");
			add(rom, "Video Capture", ctrl.video_capture_name);
			add(rom, "Screen Orientation", ctrl.screen_orientation_name);
			add(rom, "Parental Control", ctrl.parental_control_flags.join(", "));
			add(rom, "Add-on Policy", ctrl.addon_install_policy_name);
			add(
				rom,
				"Save Data",
				[
					ctrl.user_account_save && `${formatBytes(ctrl.user_account_save)} user`,
					ctrl.device_save && `${formatBytes(ctrl.device_save)} device`,
					ctrl.bcat_save && `${formatBytes(ctrl.bcat_save)} bcat`,
				]
					.filter(Boolean)
					.join(" · "),
			);
		}
		if (full?.related_titles.length) {
			add(
				rom,
				"Bundled",
				full.related_titles.map((r) => `${r.title_id_hex} (${contentTypeDisplayName(r.kind)} v${r.version})`).join(", "),
			);
		}
		if (!full) add(rom, "Keys", "Provide prod.keys to read title, icon, and content metadata");
		return {
			container,
			rom,
			innerTitle: "NCA Files",
			innerFiles: info.files.map((f) => ({
				name: f.name,
				detail: f.partition ? `${formatBytes(f.size)} · ${f.partition}` : formatBytes(f.size),
			})),
		};
	},
	title: (info) =>
		englishFirst(info.full?.control?.titles, (t) => t.language)?.name || info.container_kind.toUpperCase(),
	size: (info) => info.physical_bytes,
	console: () => "SWITCH",
	format: (info) => info.container_kind.toUpperCase(),
	meta(info) {
		const ctrl = info.full?.control;
		return [
			englishFirst(ctrl?.titles, (t) => t.language)?.publisher,
			ctrl?.display_version && `v${ctrl.display_version}`,
		];
	},
	stats: (info) => [
		...(info.full ? [{ label: "Title ID", value: info.full.application_title_id_hex }] : []),
		{ label: "NCA files", value: String(info.nca_names.length) },
		...(info.is_compressed ? [{ label: "Compressed", value: "zstd", color: "green" as const }] : []),
	],
	titleId: (info) => info.full?.application_title_id_hex ?? "",
};
