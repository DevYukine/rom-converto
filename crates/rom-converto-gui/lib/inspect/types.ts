import type { InfoResult } from "~/types/info";

export type InfoKind = InfoResult["kind"];
export type InfoOf<K extends InfoKind> = Extract<InfoResult, { kind: K }>;

export interface InspectField {
	label: string;
	value: string;
}

export interface InnerFile {
	name: string;
	detail: string;
}

export interface InspectView {
	container: InspectField[];
	rom: InspectField[];
	innerTitle: string;
	innerFiles: InnerFile[];
	hashes: InspectField[];
	contentType: string | null;
}

// What a kind module contributes to the view; omitted sections default to empty.
export interface InspectBuild {
	container?: InspectField[];
	rom?: InspectField[];
	innerTitle?: string;
	innerFiles?: InnerFile[];
	hashes?: InspectField[];
}

export interface Stat {
	label: string;
	value: string;
	color?: "t3" | "blue" | "green" | "yellow";
}

// Everything the inspect UI needs to know about one InfoResult kind. Adding a
// console means adding one module here and one entry in ./index.
export interface KindModule<K extends InfoKind> {
	build(info: InfoOf<K>): InspectBuild;
	title(info: InfoOf<K>): string;
	size(info: InfoOf<K>): number;
	console(info: InfoOf<K>): string;
	format(info: InfoOf<K>): string;
	/** Physical medium; omitted for cartridges and digital packages. */
	media?(info: InfoOf<K>): string | null;
	meta?(info: InfoOf<K>): (string | null | undefined)[];
	stats?(info: InfoOf<K>): Stat[];
	/** null hides the copy button; a string (even empty) shows it. */
	titleId?(info: InfoOf<K>): string | null;
}
