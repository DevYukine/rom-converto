## [0.23.1](https://github.com/DevYukine/rom-converto/compare/v0.23.0...v0.23.1) (2026-10-02)


### Bug Fixes

* **chd:** extract CD-mode CHDs to .cue and .bin instead of a .iso ([49bc51a](https://github.com/DevYukine/rom-converto/commit/49bc51a37541c7cec90d64529698c3f42bed9b50))
* **cue:** refuse cue and gdi file names outside the sheet's folder ([c8369d9](https://github.com/DevYukine/rom-converto/commit/c8369d917c3c7a7e21412fa7441d294849d1b0b5))
* **release:** pin actions and cross, attest releases, verify self-update downloads ([43e3414](https://github.com/DevYukine/rom-converto/commit/43e3414cd2168c09e5678e2a61bd61915e493bb4))



# [0.23.0](https://github.com/DevYukine/rom-converto/compare/v0.22.0...v0.23.0) (2026-10-01)


### Bug Fixes

* **archive:** find zip members stored with CP437 names ([c775a82](https://github.com/DevYukine/rom-converto/commit/c775a820461de6ba45c5f9562b90a8924257bd61))
* **archive:** refuse drive-relative member names on Windows ([53090fc](https://github.com/DevYukine/rom-converto/commit/53090fc6faac6640be45a45fababab8d0af91418))
* **chd:** compress every bin of a multi-file cue and store INDEX 00 pregaps like chdman ([5fe7a1a](https://github.com/DevYukine/rom-converto/commit/5fe7a1abbfba84ab3ad37ace298b06282b398a0e))
* **chd:** store and regenerate cue POSTGAP like chdman ([f466a4d](https://github.com/DevYukine/rom-converto/commit/f466a4d9539e3667c34c6e34cd1dcfcef7f34875))
* **ctr:** cap the ExeFS icon read at the SMDH size ([fefa70b](https://github.com/DevYukine/rom-converto/commit/fefa70b8225da76a5791b97e746a442d64d6f37c))
* **ctr:** stop corrupting CIA contents after the first on decrypt and encrypt ([40f89a1](https://github.com/DevYukine/rom-converto/commit/40f89a1116abf569fb1d576957760026c49f811f))
* **dat:** count a cue track once when its path has redundant parts ([393e083](https://github.com/DevYukine/rom-converto/commit/393e083f37cb9c6f282383ef711fff637120e7fe))
* **dol,rvl:** decode Japanese banner text, game names and file names ([296c002](https://github.com/DevYukine/rom-converto/commit/296c0025e8416145c08b1ea8276312a572a59617))
* **dol,rvl:** fail verify on a broken RVZ container and show why ([451a219](https://github.com/DevYukine/rom-converto/commit/451a219c81d6593241e38a83fd8c649984187641))
* **dol:** label BNR2 banner slots English through Dutch ([0b87ba5](https://github.com/DevYukine/rom-converto/commit/0b87ba522839d763d54b7a840c25b27cb4dae35f))
* **fds:** label the 16-byte header as FDS header instead of fwNES ([2b7924a](https://github.com/DevYukine/rom-converto/commit/2b7924a51560ec352a7a374ca349e0db65a285ca))
* **gui:** refuse updates while jobs run and clean up a failed portable swap ([f31604e](https://github.com/DevYukine/rom-converto/commit/f31604eb4be555e3f0e135858fe0488a25148846))
* **hash:** check cancellation before every read ([abd1443](https://github.com/DevYukine/rom-converto/commit/abd144371455f9bfc67d99ccb69ab424d378d892))
* **lib:** use as_chunks for constant chunk sizes ([c0d23b9](https://github.com/DevYukine/rom-converto/commit/c0d23b9a88d75a7a4176d64ba47eb2c8f0b01290))
* **lib:** write outputs with the default file mode ([2ca4591](https://github.com/DevYukine/rom-converto/commit/2ca4591c3beaf52515df8e157eceea47d13087e8))
* **organize:** delete sources only when the output exists and keep writing playlists after one fails ([55e19d1](https://github.com/DevYukine/rom-converto/commit/55e19d12bd5bca9d66a61f61d39463c71cb6a969))
* **organize:** keep files that are also outputs or inputs under another path spelling ([fd544d4](https://github.com/DevYukine/rom-converto/commit/fd544d4e5522fdf35e16449119dcbbbaac327e4a))
* **runner:** keep outputs that cannot be verified under overwrite-invalid ([abd442b](https://github.com/DevYukine/rom-converto/commit/abd442b95052976ac4aa5f19d73390b07ade3727))
* **rvz:** reject truncated and malformed RVZ containers ([a7f795c](https://github.com/DevYukine/rom-converto/commit/a7f795c1cc98e0a7130ad23d98c3e4d7a644fc0d))
* **wup:** refuse title file names that would write outside the output folder ([9d9d13c](https://github.com/DevYukine/rom-converto/commit/9d9d13c6f4f7b128d33cbf6df9ba17f79acf0226))


### Features

* **dat:** parse regions, languages and release types from DAT names ([3a86af5](https://github.com/DevYukine/rom-converto/commit/3a86af5a6dd8603cba9e0ab67f25adf09d885206))
* **gui:** update installed and portable copies with the package they run on every platform ([c7665a6](https://github.com/DevYukine/rom-converto/commit/c7665a6aafe162f313e7dc08523f6af933552fcc))
* **organize:** add filters, best release per game, letter folders, patching and clean ([d344c52](https://github.com/DevYukine/rom-converto/commit/d344c52b1059032ce8568323366258bfc9e4588a))
* **organize:** sort a rom library into per-console folders and compress each file into its best format across lib, cli and gui ([3329129](https://github.com/DevYukine/rom-converto/commit/33291290c72d578f7c4546f4227c499f57a45cea))
* **patch:** apply IPS, UPS, BPS, APS, PPF, RUP and VCDIFF patches ([fe5b955](https://github.com/DevYukine/rom-converto/commit/fe5b9552ffc539b9dbdd2c9151afe72606a3fe9d))
* **template:** add frontend folder, DAT and input folder tokens ([d03f79e](https://github.com/DevYukine/rom-converto/commit/d03f79e8f9459089f2cdd0af1991937f02720fc0))
* **wup:** convert wud disc images to wux and back across lib, cli, gui and ffi ([3eefd94](https://github.com/DevYukine/rom-converto/commit/3eefd9462572b152c2cf1013b3297bcacfa49eb2))
* **zip:** write and validate TorrentZip and RVZSTD archives ([0a01859](https://github.com/DevYukine/rom-converto/commit/0a018599f0fd97218d7aa0c537e90fee2d28e8d7))


### Performance Improvements

* **cartridges:** header range reads and streamed checksums ([c84c38a](https://github.com/DevYukine/rom-converto/commit/c84c38a943067ff67156c4d9c99628dc437679c1))
* **chd,cso:** map-only open, extent checks, streamed oversized blocks ([d5fa8a4](https://github.com/DevYukine/rom-converto/commit/d5fa8a4d2a1807d0fccf9086f504ab599266199a))
* **ctr:** inspect z3ds files through the seek table and read only the exefs icon instead of decompressing the whole rom ([635ed59](https://github.com/DevYukine/rom-converto/commit/635ed591f3c429756e788216bad9f50a4634141b))
* **ctr:** stream cia decrypt and z3ds frames ([813ebfb](https://github.com/DevYukine/rom-converto/commit/813ebfbab11248e548dd34a8bd276c67f587eb6a))
* **dat:** paged library scans ([6efd41d](https://github.com/DevYukine/rom-converto/commit/6efd41d79e149eb6cf753f2332373854e905e5df))
* **disc:** stream rvz, wia, gcz and nkit within bounded buffers ([8fe4bdb](https://github.com/DevYukine/rom-converto/commit/8fe4bdb0374a993ee9b2cd5d84455e9a842e4afe))
* **hash:** compute CRC32 with a slice-by-16 table ([072ea97](https://github.com/DevYukine/rom-converto/commit/072ea97eac7df502ee624527fbe88d237ccb657e))
* **microsoft:** parse xbe, xex and xdbf by range ([8187f57](https://github.com/DevYukine/rom-converto/commit/8187f57ff8c29c51db9b9492a18d287edaef922d))
* **nx:** bound ncz decoding and validate tables before allocating ([50d1a3c](https://github.com/DevYukine/rom-converto/commit/50d1a3c0f21dbc2eb0d04d0c6de846bf32c77793))
* **sony:** batched pkg item reads ([4254135](https://github.com/DevYukine/rom-converto/commit/42541352a3e1dcda18109d93f03ad90d2abf4604))
* **util:** budgeted worker admission and bounded group reader ([f494dac](https://github.com/DevYukine/rom-converto/commit/f494dacbd19eccefe1eb91ac1680350bcdad422e))
* **wup:** decrypt by range and stream titles into wua ([34d6133](https://github.com/DevYukine/rom-converto/commit/34d613346a5aebb06f2c9e6954138ceed0d7adc9))
* **zar:** bound writer buffers and reorder queue ([08b007b](https://github.com/DevYukine/rom-converto/commit/08b007b093374617f2a57a40a3da65f1e606b6fb))



# [0.22.0](https://github.com/DevYukine/rom-converto/compare/v0.21.0...v0.22.0) (2026-09-11)


### Features

* **chd:** inspect CHD v1-v4 and migrate them to v5 across lib, CLI and GUI ([6cfa287](https://github.com/DevYukine/rom-converto/commit/6cfa2870fcb5b74fab64b180114fa868b0162488))
* **ctr:** add a trim option to cia to 3ds conversion and size the free space check from the real cci output ([008d80e](https://github.com/DevYukine/rom-converto/commit/008d80eef5ca69440f9ba8010fe57b5aa74dbaa6))
* **gui:** add disc conversion logo ([b8799f1](https://github.com/DevYukine/rom-converto/commit/b8799f1f304deddf84499d38cd75d6cf61562f1e))
* **gui:** count files instead of bytes on the dat scan bar, add rate, eta, phases, relative paths, filtering and a virtual result list ([933fa9e](https://github.com/DevYukine/rom-converto/commit/933fa9e24c9ca58012c05d0738c12d7b57ce26a8))
* **gui:** show an update notice with periodic checks and harden the cli self-update swap ([3eb004f](https://github.com/DevYukine/rom-converto/commit/3eb004f215071b90d181e9666c0d77c49dccbeb7))
* **nx:** merge and split nsp/xci into super containers ([c07c464](https://github.com/DevYukine/rom-converto/commit/c07c464a574f79c766cf9b84166c747b2eea3ee2))
* **runner:** route the cli and gui through the lib runner, add the missing ops and consolidated dat logic, generate gui types from rust and fold dat pages plus verify-after results into the gui registry ([c1ccd2e](https://github.com/DevYukine/rom-converto/commit/c1ccd2e711facef1abc096c350e1885113d574c1))
* **sony:** inspect ps4 and ps5 pkg files without keys and label ps3 pkg content types across lib, cli and gui ([1e78d0e](https://github.com/DevYukine/rom-converto/commit/1e78d0eb617461ed8a1f2aaf5052ce8de7fcf37c))
* **xenon:** convert xbox 360 disc images to games on demand containers ([ea2d52f](https://github.com/DevYukine/rom-converto/commit/ea2d52f30e70ae11ab2aacef9a650e9f0f719595))



# [0.21.0](https://github.com/DevYukine/rom-converto/compare/v0.20.0...v0.21.0) (2026-09-03)


### Bug Fixes

* **gui:** size cue and folder inputs by content so queue savings are correct ([c06f019](https://github.com/DevYukine/rom-converto/commit/c06f01932856db349581faca146725c9214f55d8))
* **nx:** emit NCZBLOCK version 2 type 1 to match nsz and cover keep-style xcz decompress ([aebeeef](https://github.com/DevYukine/rom-converto/commit/aebeeef164b7d7ed0701a0fbcdfc9557b9891f1a))


### Features

* **chd:** create laserdisc chds from avi with auto-detect and ld info ([61c7e7a](https://github.com/DevYukine/rom-converto/commit/61c7e7ab805dc7c5e08a0e14afce909df1277eb8))
* **cli:** add batch info mode and capabilities manifest ([9be637e](https://github.com/DevYukine/rom-converto/commit/9be637ebbc6c4ce04a892c4a283b7f43ae693836))
* **info:** normalize content type across consoles, route dax/3dsx/gcz/wia and inspect PSP/PS3/Vita pkg with icons ([0f303e2](https://github.com/DevYukine/rom-converto/commit/0f303e2770f25a2daff70e346e7f1ec12273d11f))
* **info:** split disc console/media with a media chip across consoles and decrypt vita pkg artwork through pfs with a neighboring license ([ddf7608](https://github.com/DevYukine/rom-converto/commit/ddf7608a379d9419cf2fddfc2cc807dfd284617d))
* **nds:** add DS cartridge info across lib, CLI and GUI ([8b90185](https://github.com/DevYukine/rom-converto/commit/8b90185cc813b5a913e30a8b13f4996e1d2ebb88))
* **nds:** add DS secure area encryption and decryption across lib, CLI and GUI ([7f85d21](https://github.com/DevYukine/rom-converto/commit/7f85d214c59224ad167cddb79be41dea7382e267))
* **progress:** report cumulative completion fraction on advance events ([d39af46](https://github.com/DevYukine/rom-converto/commit/d39af46a1e1b4151d66c921e0bda7a398409bf3a))
* **psp:** accept PSN pkg input for to-iso via a seekable decrypted package item reader ([885a958](https://github.com/DevYukine/rom-converto/commit/885a9587e92f93aec6ab2981e4486e49ce48f572))
* **psp:** add PBP EBOOT info and segment extraction across lib, CLI and GUI ([6bd4cd5](https://github.com/DevYukine/rom-converto/commit/6bd4cd5a9a6fe0eedc7e96d1588ddc0d0f99fa12))
* **psp:** convert NPUMDIMG EBOOT.PBP to ISO with kirk and amctrl decryption across lib, CLI and GUI ([a0aaba4](https://github.com/DevYukine/rom-converto/commit/a0aaba4f27625628a80354e9f202c51a048d43a3))
* **psp:** read psp/ps3 pkg title from the item param.sfo and extract pic1/pic0 as background ([c20d1b8](https://github.com/DevYukine/rom-converto/commit/c20d1b8e5bbe552a0de5ff362bf4b9792f8fb928))
* **retro:** add cartridge-era console inspection across lib, CLI and GUI ([427fb40](https://github.com/DevYukine/rom-converto/commit/427fb40c9e595ddf55f7cc470ddbe496e17171a9))
* **retro:** add Sega Saturn, Sega CD, Dreamcast, 32X and FDS inspection with Sega-aware cue and iso routing ([62ee28f](https://github.com/DevYukine/rom-converto/commit/62ee28f2349143e0abebab4fe46619a77b16abc6))
* **rvl:** render the opening.bnr channel banner from brlyt layout, brlan animation and tpl textures for the inspect image ([7f4c4c1](https://github.com/DevYukine/rom-converto/commit/7f4c4c109b8ec3f3d9eafedf7cec8be4b35067ca))
* **vita:** add VPK, PKG and NoNpDrm support across lib, CLI and GUI ([04e7a4b](https://github.com/DevYukine/rom-converto/commit/04e7a4b4139759f58df3899e08f75ab9aa972e1e))


### Performance Improvements

* **nx:** peek nca content type before opening ncz in info control scan ([5183d4f](https://github.com/DevYukine/rom-converto/commit/5183d4f070f6f65e55d1aa55a132c3c26e300989))



# [0.20.0](https://github.com/DevYukine/rom-converto/compare/v0.19.0...v0.20.0) (2026-09-02)


### Bug Fixes

* **gui:** compute queue drawer MB/s from elapsed time instead of bytes done ([fdc7d2b](https://github.com/DevYukine/rom-converto/commit/fdc7d2bef56cf2176ff276e82be9b10bac0d33e6))


### Features

* **info:** overhaul inspect view with uniform sections, inner files, icons, and encryption state ([41864bc](https://github.com/DevYukine/rom-converto/commit/41864bc1fbe1a1088865b66dbc83c350865136a7))


### Performance Improvements

* **chd:** disable flacenc per-call thread spawning in flac hunk trials ([fc5b111](https://github.com/DevYukine/rom-converto/commit/fc5b1113acb027f5391c82176393c534ab5239d5))



