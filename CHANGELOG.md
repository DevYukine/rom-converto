# [0.24.0](https://github.com/DevYukine/rom-converto/compare/v0.23.2...v0.24.0) (2026-10-11)


### Bug Fixes

* **archive:** cap the members an archive listing keeps ([34b2b37](https://github.com/DevYukine/rom-converto/commit/34b2b37ba8c0d1bc2dd6b2bcc2c4eda6f166857f))
* **archive:** extract tar and rar members by index ([2369156](https://github.com/DevYukine/rom-converto/commit/2369156f1c5e8543e92608752a67ac83b1a41040))
* **archive:** read a member that is not first in a solid 7z ([d32ac1e](https://github.com/DevYukine/rom-converto/commit/d32ac1e0a9845f1fc9c0b9164fa9138d9dbc9c4b))
* **archive:** refuse 7z archives whose decoder would need more than 1536 MiB ([3f82b1f](https://github.com/DevYukine/rom-converto/commit/3f82b1fd5a1a83dbe7c0815e069b40d70fd769a4))
* **archive:** report an unreadable archive in a recursive walk as skipped unless its name claims an image ([2829862](https://github.com/DevYukine/rom-converto/commit/28298621b263928acbb04e25554caa0767b60215))
* **archive:** skip an archive whose member also sits unpacked beside or under it ([99c3961](https://github.com/DevYukine/rom-converto/commit/99c396169c36dfe985aa2db789ffe7932ab44e72))
* **archive:** stop staging a member past its declared size ([916f70c](https://github.com/DevYukine/rom-converto/commit/916f70c4d3fa14067faa6e63def15da3daff01e0))
* **chd:** exit non-zero when a recursive verify or in-place migrate fails a file ([4dd7cdf](https://github.com/DevYukine/rom-converto/commit/4dd7cdfc9ebe55aaeb309f26d62cd886a3ba950d))
* **chd:** honor gd-rom pad frames when extracting, hashing and inspecting chds ([dc4020d](https://github.com/DevYukine/rom-converto/commit/dc4020d7b8f344cdfb89abbae8c6206c374899b9))
* **chd:** validate unit size and reject oversubscribed map trees ([f3b3c0c](https://github.com/DevYukine/rom-converto/commit/f3b3c0cad65394db7c2820125c65ddf57b4166a1))
* **ctr:** copy twl content with the full 64-bit size ([2a7fdeb](https://github.com/DevYukine/rom-converto/commit/2a7fdeba5b9cadf4a50c9d2dc666ec7efea59803))
* **ctr:** flag title-key encrypted cias and boot a cxi only from a bcxi in bundle info ([df9199a](https://github.com/DevYukine/rom-converto/commit/df9199aa524a852dc30a13ac68b6eb7e6272deef))
* **ctr:** stage in-place rewrites under a temp name and accept read-only inputs ([69bee99](https://github.com/DevYukine/rom-converto/commit/69bee990526452afaa4ca99d6e4814987c0197fa))
* **ctr:** validate tmd content info ranges before rebuilding hashes ([f205046](https://github.com/DevYukine/rom-converto/commit/f205046ea5290ff75f79f062c3584d57e1867d11))
* **cue:** write the extracted iso through a scratch file and honor cancel ([30a08fa](https://github.com/DevYukine/rom-converto/commit/30a08fa4d1e20b68e49abe2662c9acbef008c721))
* **gui:** accept iso on the psp and ps2 convert page and compress it to chd ([fe8ae77](https://github.com/DevYukine/rom-converto/commit/fe8ae776ea67796947bf4cb6cc85dc36dabc5e2e))
* **gui:** carry the output directory into templated jobs ([3fbabdf](https://github.com/DevYukine/rom-converto/commit/3fbabdf7ea3a99da738a67a472f7d4d18aa9ba7e))
* **gui:** drop per-member output hints on the bundle page ([3e1dca1](https://github.com/DevYukine/rom-converto/commit/3e1dca11e001a2dbac2535f59f1d62fac7d791a6))
* **gui:** echo every staged input on the merge and bundle pages ([d49c591](https://github.com/DevYukine/rom-converto/commit/d49c591a6b168f0a3bc5739b3037090ef3571088))
* **gui:** echo the conflict policy the job sends ([c31369c](https://github.com/DevYukine/rom-converto/commit/c31369c97918644dbedf61d7c806b5c9fc46731b))
* **gui:** keep modal text in the theme color ([a8d0c72](https://github.com/DevYukine/rom-converto/commit/a8d0c72540cb26769c5b6f6eb1971d744d792be9))
* **gui:** keep preset list fields editable while typing ([8ad5429](https://github.com/DevYukine/rom-converto/commit/8ad5429aeb80c4925074780d15e4eb9f570296ad))
* **gui:** keep truncated paths reading left to right ([94ce606](https://github.com/DevYukine/rom-converto/commit/94ce606f223fc751fc6d26e434add5f66761a1f6))
* **gui:** let the runner name archive outputs on the ctr, ntr and nx pages ([128c25c](https://github.com/DevYukine/rom-converto/commit/128c25c07f05b598c5610d846c9c37d7fbc1fb06))
* **gui:** let the runner name nxemu outputs of archived and templated inputs ([108dd12](https://github.com/DevYukine/rom-converto/commit/108dd126d3ad464f8313555e8e9b9e631e00ae36))
* **gui:** show the real default folder on the nx split and ctr unbundle pages ([7b301f0](https://github.com/DevYukine/rom-converto/commit/7b301f0e37f33796ef2b3c2a4f072380e27596e3))
* **gui:** stop offering rename on directory output pages ([8d0f570](https://github.com/DevYukine/rom-converto/commit/8d0f5703baad1b945d24c7b78206924b61cd130b))
* **nx:** bound pfs0 and hfs0 name offsets and decoded name size ([66846e2](https://github.com/DevYukine/rom-converto/commit/66846e24a205051f64bcc7b45790bb6e88a4fa00))
* **nx:** cap and deduplicate xci root partitions when walking containers ([0534f22](https://github.com/DevYukine/rom-converto/commit/0534f22a6d0f582863e485a2b360355be719b534))
* **nx:** hash rewritten nca prefixes in chunks and refuse entries past the end of the file when decrypting ([7cfb46f](https://github.com/DevYukine/rom-converto/commit/7cfb46f90b47d16ed743064b47ba3244c7e13b57))
* **nx:** reject overflowing romfs extents and cyclic hash chains ([4b2a99e](https://github.com/DevYukine/rom-converto/commit/4b2a99e78933e9b0b532a85b618da5ec412470ba))
* **nx:** stage split outputs and publish them together ([8805ba4](https://github.com/DevYukine/rom-converto/commit/8805ba48d5486f99d18ece0c73eee1a528e74480))
* **nx:** strip compound suffixes without splitting utf-8 names ([7ee7146](https://github.com/DevYukine/rom-converto/commit/7ee7146d97367c7034fc68db946c0b6d98d54223))
* **organize:** anchor clean deletions and backups to directory handles ([66ee418](https://github.com/DevYukine/rom-converto/commit/66ee418a4ceec8a267f7e50fd8afbe1423003e00))
* **organize:** use a plain rename for backups on platforms without no-replace rename ([ec10217](https://github.com/DevYukine/rom-converto/commit/ec102170587410ea82ac42dba500d1de44aba133))
* **psp:** write the extracted iso through a scratch file and honor cancel ([5eac564](https://github.com/DevYukine/rom-converto/commit/5eac5641a38b4209b0573f33f4853f6d38817cd6))
* **runner:** let overwrite reruns regenerate walked outputs once their source was read ([0ebe48b](https://github.com/DevYukine/rom-converto/commit/0ebe48b5f3ff65c0d61b84e089f5cd21438c07eb))
* **runner:** refuse a directory output path inside a directory input ([d6a0102](https://github.com/DevYukine/rom-converto/commit/d6a01026f6513612c7bd59a068340bfb17015129))
* **runner:** refuse a directory output that is its own input instead of deleting it ([3a2cd5a](https://github.com/DevYukine/rom-converto/commit/3a2cd5a0eb3ffbbdfa4d3363b938291dbdb74b56))
* **runner:** run hash and fixdat writes on the blocking pool ([7f181db](https://github.com/DevYukine/rom-converto/commit/7f181db9f19b185497239d5f6f7a231479657fe4))
* **rvl:** bound u8 directory ends and listed path size ([3e10514](https://github.com/DevYukine/rom-converto/commit/3e105146a63e42373bdf05d1f5ac6d6695f8ad73))
* **rvl:** clamp banner texture coordinates before sampling ([f63c521](https://github.com/DevYukine/rom-converto/commit/f63c5210265a12a25749612d76e594eb90d89976))
* **util:** keep a panicked worker pool from hanging its readers ([8394481](https://github.com/DevYukine/rom-converto/commit/839448196e1b91a65be6a6e07f733599410eac54))
* **util:** write scratch outputs through the reserved file handle ([91dad2a](https://github.com/DevYukine/rom-converto/commit/91dad2abc98414a7f249a55a72c4d1d8d8e84c24))
* **vita:** reject invalid utf-8 and duplicate pkg item paths ([4133824](https://github.com/DevYukine/rom-converto/commit/413382436d8b4a9162ccfb3e92b0fee782d14bde))
* **wup:** cap decoded wua block size on the multi-frame path ([0ff93ac](https://github.com/DevYukine/rom-converto/commit/0ff93aca6974775154bf25ca0a251edb5a3dd874))
* **wup:** read the company code prefix on a char boundary ([2e51991](https://github.com/DevYukine/rom-converto/commit/2e519917437cf6bf7c5232e53a06f4e9119e4587))
* **xbox:** cap directory nesting depth ([5bb90a9](https://github.com/DevYukine/rom-converto/commit/5bb90a955a88039583238959ea4277747b874dbd))
* **xenon:** refuse symlinked folders in the games on demand output tree ([a175704](https://github.com/DevYukine/rom-converto/commit/a175704e3a5444faacd4b749f8163fe427c8e153))
* **zar:** reject case-insensitive duplicate entries when writing ([4c17fe5](https://github.com/DevYukine/rom-converto/commit/4c17fe5269882eb5c0ad5d0bc3b7d89e471809a8))
* **zar:** reject invalid utf-8 entry names and duplicate extraction paths ([9935929](https://github.com/DevYukine/rom-converto/commit/993592923e215f4385fa8d4d4ac679a213040968))


### Features

* **chd:** compress dreamcast gdi sets into gd-rom chds across lib, cli and gui ([cedb26b](https://github.com/DevYukine/rom-converto/commit/cedb26b7104614033312594d159a83af27bf3b67))
* **ctr:** bundle and unbundle Azahar .bcia/.bcci/.bcxi roms and inspect bundled titles across lib, cli and gui ([33dcce4](https://github.com/DevYukine/rom-converto/commit/33dcce476a0824cca25e946859e5e99d4a5d4a0a))
* **gui:** polish layouts, pickers and result views across every page and window size ([519125a](https://github.com/DevYukine/rom-converto/commit/519125a94cbfe82489ddb137fdcbb255f97d4d69))
* **nx:** decrypt nsp and xci into nxemu dnsp and dxci across lib, cli and gui ([4c26447](https://github.com/DevYukine/rom-converto/commit/4c264475a8098bd44c3a333a1dd31db188d99dba))
* open archives in recursive batch runs and add -R to nx split and xenon convert ([d4fdc75](https://github.com/DevYukine/rom-converto/commit/d4fdc75c750ec3c461277f07ef18b65093cda6b9))
* **organize:** place multi-disc sets in per-game folders with --multi-disc-dirs ([25411a6](https://github.com/DevYukine/rom-converto/commit/25411a63a6a8ec1a7a16ac64888ab7a2e3a9b786))



## [0.23.2](https://github.com/DevYukine/rom-converto/compare/v0.23.1...v0.23.2) (2026-10-02)


### Bug Fixes

* **chd:** extract CD images to one bin per track ([97b8ae3](https://github.com/DevYukine/rom-converto/commit/97b8ae3d01c6b63c0d9d032221524a286f7b3dd9))



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



