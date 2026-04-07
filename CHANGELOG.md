# [0.4.0](https://github.com/DevYukine/rom-converto/compare/v0.3.2...v0.4.0) (2026-04-07)


### Bug Fixes

* **chd:** fix ECC computation for Mode 2 CD sectors ([c59154b](https://github.com/DevYukine/rom-converto/commit/c59154bd04e7ca5886acabf9d002f1a46634899b))
* **chd:** fix map decompression RLE off-by-one ([5971338](https://github.com/DevYukine/rom-converto/commit/5971338562c9ac7d269a54caf9ab349ddd666a3a))
* **chd:** fix MSF lead-in subtraction causing overflow ([955a436](https://github.com/DevYukine/rom-converto/commit/955a436ee181a6f71a26c73f9cef3cdfbaac8c20))
* **chd:** match chdman LZMA format and dict size ([e56b2b3](https://github.com/DevYukine/rom-converto/commit/e56b2b390dcc02bb1c36dd9535b64819a5c827f9))


### Features

* add progress bar for chd conversion ([ae845d9](https://github.com/DevYukine/rom-converto/commit/ae845d9133bdb93d1cd1451cb785f6fa2d3f9e43))
* chd v5 ([523b29f](https://github.com/DevYukine/rom-converto/commit/523b29fbb007779f2957c5dd5b0802928e168674))
* **chd:** add ECC stripping for Mode 1 CD sectors ([96926ae](https://github.com/DevYukine/rom-converto/commit/96926ae8224945ad36681ea1abe422f9b6e7b9c7))
* **chd:** implement verify & extract command ([88e0f79](https://github.com/DevYukine/rom-converto/commit/88e0f79ce681ca2f2dac2f685e88954143640e29))
* **chd:** switch from liblzma to lzma-sdk-sys ([5ed89de](https://github.com/DevYukine/rom-converto/commit/5ed89de6383b818c19d5b5c1cf2647fa98b55187))
* **compressors:** simplify & implement proper cd spec ([962267d](https://github.com/DevYukine/rom-converto/commit/962267df71850c7b50d8ad8e0d83e4b95d9f1114))
* **ctr:** add Azahar ctr compression format support ([9032d75](https://github.com/DevYukine/rom-converto/commit/9032d755b3734bf76f6a3fdaa982a6dae9c560a2))
* first initial setup ([434c23f](https://github.com/DevYukine/rom-converto/commit/434c23f1dad9ce0c730b808d735d5c89922aae36))


### Performance Improvements

* **chd:** parallel compression pipeline with bulk I/O ([ca351fa](https://github.com/DevYukine/rom-converto/commit/ca351fa163f9c46e7042f7cbc0b0aa080e12f531))
* **chd:** persistent codec state with reusable LZMA encoder and deflate compressors ([e7e859b](https://github.com/DevYukine/rom-converto/commit/e7e859b1f545554a24042e85e8cc432bac72ea35))
* **chd:** skip FLAC codec for data tracks ([eca97fe](https://github.com/DevYukine/rom-converto/commit/eca97fe448a0d5f297edd21aa8f5d0c4933930f9))



## [0.3.2](https://github.com/DevYukine/rom-converto/compare/v0.3.1...v0.3.2) (2026-01-12)


### Bug Fixes

* **updater:** check for correct name of outdated file ([0ec1460](https://github.com/DevYukine/rom-converto/commit/0ec1460d33d365552ee464d8925238001ef472e7))



## [0.3.1](https://github.com/DevYukine/rom-converto/compare/v0.3.0...v0.3.1) (2026-01-12)


### Bug Fixes

* **cia:** update for latest commit to ExeFSHeader changing the field names ([ae2adac](https://github.com/DevYukine/rom-converto/commit/ae2adace808f5d1bd1c1e47f17d90215913b4e99))


### Performance Improvements

* **cia:** only load seeddb.bin once in memory ([985d11c](https://github.com/DevYukine/rom-converto/commit/985d11c65bf3254c267f03e3067d2c53e9b3b6c7))



# [0.3.0](https://github.com/DevYukine/rom-converto/compare/v0.2.2...v0.3.0) (2025-07-03)


### Bug Fixes

* **ctr:** default to use latest tmd file when multiple exists ([69dde38](https://github.com/DevYukine/rom-converto/commit/69dde386e942b4b2bd76683bbc3fc914423e07a4))
* **help:** properly display heart ([cce8a78](https://github.com/DevYukine/rom-converto/commit/cce8a78a233517bfba22804c91fb7a72fec2286a))
* **logging:** default to info level if no log level is set ([9927204](https://github.com/DevYukine/rom-converto/commit/99272043e93a71d387ad90c3fb7792d3b0641fa4))


### Features

* add self-update command ([f3d98a7](https://github.com/DevYukine/rom-converto/commit/f3d98a7f81c98d3110e5154e4dd9310a7d558dbe))



## [0.2.2](https://github.com/DevYukine/rom-converto/compare/v0.2.1...v0.2.2) (2025-07-03)


### Bug Fixes

* **ci:** build without --locked for release ([07e6264](https://github.com/DevYukine/rom-converto/commit/07e62644a91631904815bb0d622f6310b6dc1bdc))



