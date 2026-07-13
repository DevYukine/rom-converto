import { describe, expect, it } from "vitest";
import {
  deriveChdPath,
  deriveCompressedPath,
  deriveConvertedPath,
  deriveCsoPath,
  deriveCuePath,
  deriveDecryptedPath,
  deriveDiscIsoPath,
  deriveEncryptedPath,
  deriveMergedCuePath,
  deriveNszPath,
  deriveRvzPath,
  deriveWuaPath,
  stripArchiveExt,
} from "./useDerivedPath";

// A stem containing dots must survive derivation intact once the archive
// extension is stripped; only a real image extension may be replaced.
const DOTTED = "F:\\roms\\10.000 Bullets (Europe) (En,Fr,De,Es,It)";

describe("archive inputs with dots in the title", () => {
  it("issue #10: converting a dotted zip name to chd keeps the full title", () => {
    expect(deriveChdPath("10.000 Bullets (Europe) (En,Fr,De,Es,It).zip")).toBe(
      "10.000 Bullets (Europe) (En,Fr,De,Es,It).chd",
    );
  });

  it("derives chd from a dotted zip name without truncating", () => {
    expect(deriveChdPath(`${DOTTED}.zip`)).toBe(`${DOTTED}.chd`);
  });

  it("derives cso/cue/iso from dotted archive names without truncating", () => {
    expect(deriveCsoPath(`${DOTTED}.7z`, "cso")).toBe(`${DOTTED}.cso`);
    expect(deriveCuePath(`${DOTTED}.rar`)).toBe(`${DOTTED}.cue`);
    expect(deriveDiscIsoPath(`${DOTTED}.zip`)).toBe(`${DOTTED}.iso`);
  });

  it("derives compressed/converted defaults without truncating", () => {
    expect(deriveCompressedPath(`${DOTTED}.zip`)).toBe(`${DOTTED}.z3ds`);
    expect(deriveConvertedPath(`${DOTTED}.zip`)).toBe(`${DOTTED}.out`);
    expect(deriveNszPath(`${DOTTED}.zip`)).toBe(`${DOTTED}.nsz`);
    expect(deriveRvzPath(`${DOTTED}.zip`)).toBe(`${DOTTED}.rvz`);
  });

  it("derives decrypted/encrypted names without truncating", () => {
    expect(deriveDecryptedPath(`${DOTTED}.zip`)).toBe(`${DOTTED}.decrypted.cia`);
    expect(deriveEncryptedPath(`${DOTTED}.zip`)).toBe(`${DOTTED}.encrypted.cia`);
  });

  it("keeps single-letter title tails (Super Mario Bros. U)", () => {
    const name = "D:/wiiu/Super Mario Bros. U";
    expect(deriveChdPath(`${name}.zip`)).toBe(`${name}.chd`);
  });
});

describe("plain image inputs keep extension replacement", () => {
  it("replaces a real image extension", () => {
    expect(deriveChdPath("game.iso")).toBe("game.chd");
    expect(deriveChdPath("game.cue")).toBe("game.chd");
    expect(deriveCompressedPath("game.cia")).toBe("game.zcia");
    expect(deriveNszPath("game.xci")).toBe("game.xcz");
  });

  it("replaces the inner extension when the archive name carries one", () => {
    expect(deriveChdPath("game.iso.zip")).toBe("game.chd");
  });

  it("drops a bare trailing dot instead of doubling it", () => {
    expect(deriveChdPath("Game.")).toBe("Game.chd");
  });

  it("still replaces the extension of dotted names with a real extension", () => {
    expect(deriveChdPath(`${DOTTED}.iso`)).toBe(`${DOTTED}.chd`);
    expect(deriveDecryptedPath(`${DOTTED}.cia`)).toBe(`${DOTTED}.decrypted.cia`);
  });
});

describe("merged cue derivation", () => {
  it("keeps dots in the stem while stripping the track tag", () => {
    expect(deriveMergedCuePath("10.000 Bullets (Track 1).cue")).toBe(
      "10.000 Bullets (merged).cue",
    );
  });
});

describe("wua derivation", () => {
  it("keeps dots inside the title", () => {
    expect(deriveWuaPath("D:\\wiiu\\Super Mario Bros. U (Europe)")).toBe(
      "D:\\wiiu\\Super Mario Bros. U.wua",
    );
  });
});

describe("stripArchiveExt", () => {
  it("strips single and double archive extensions", () => {
    expect(stripArchiveExt("game.zip")).toBe("game");
    expect(stripArchiveExt("game.tar.gz")).toBe("game");
    expect(stripArchiveExt("game.iso")).toBe("game.iso");
  });
});
