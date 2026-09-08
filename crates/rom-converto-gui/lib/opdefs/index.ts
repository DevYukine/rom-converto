export * from "./types";

import { registerOps, type OpDef } from "./types";
import { compressOps } from "./compress";
import { extractOps } from "./extract";
import { decryptOps } from "./decrypt";
import { encryptOps } from "./encrypt";
import { convertOps } from "./convert";
import { verifyOps } from "./verify";
import { datOps } from "./dat";
import { toolOps } from "./tools";

// One entry per op module. A module left out of this array no longer registers
// silently: its export is unused and the op is missing from the registry.
const OP_MODULES: OpDef[][] = [
	compressOps,
	extractOps,
	decryptOps,
	encryptOps,
	convertOps,
	verifyOps,
	datOps,
	toolOps,
];

for (const defs of OP_MODULES) registerOps(defs);
