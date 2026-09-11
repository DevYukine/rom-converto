import { chd } from "./chd";
import { cso } from "./cso";
import { ctr } from "./ctr";
import { dol } from "./dol";
import { laserDisc } from "./laser-disc";
import { ntr } from "./ntr";
import { nx } from "./nx";
import { pbp } from "./pbp";
import { pkg } from "./pkg";
import { ps3 } from "./ps3";
import { ps4Pkg } from "./ps4-pkg";
import { ps5Pkg } from "./ps5-pkg";
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
	ps4_pkg: ps4Pkg,
	ps5_pkg: ps5Pkg,
	psx,
	psp,
	laser_disc: laserDisc,
	ntr,
	retro,
	pbp,
	vpk,
	pkg,
};
