import { chd } from "./chd";
import { cso } from "./cso";
import { ctr } from "./ctr";
import { dol } from "./dol";
import { laserDisc } from "./laser-disc";
import { nds } from "./nds";
import { nx } from "./nx";
import { pbp } from "./pbp";
import { pkg } from "./pkg";
import { ps3 } from "./ps3";
import { psp } from "./psp";
import { psx } from "./psx";
import { retro } from "./retro";
import { rvl } from "./rvl";
import { vpk } from "./vpk";
import { wup } from "./wup";
import { xbox } from "./xbox";
import { xenon } from "./xenon";
import type { InfoKind, KindModule } from "./types";

// One entry per InfoResult kind. A kind left out is a type error, so adding a
// console is adding a module here and nowhere else.
export const kindModules: { [K in InfoKind]: KindModule<K> } = {
	chd,
	cso,
	ctr,
	dol,
	rvl,
	wup,
	nx,
	xbox,
	xenon,
	ps3,
	psx,
	psp,
	laser_disc: laserDisc,
	nds,
	retro,
	pbp,
	vpk,
	pkg,
};
