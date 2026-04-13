# [0.6.0](https://github.com/DevYukine/rom-converto/compare/v0.5.2...v0.6.0) (2026-04-13)


### Features

* **ctr:** add 3ds/ncsd and compressed z3ds verification support ([9cb1ff1](https://github.com/DevYukine/rom-converto/commit/9cb1ff156a78412a871f2f24ad039b8152f0302c))


### Performance Improvements

* **ctr:** stream verify, compress, and CIA write to reduce memory usage ([285db21](https://github.com/DevYukine/rom-converto/commit/285db21168e37ec1b988556bca0a700f6463a53c))



## [0.5.2](https://github.com/DevYukine/rom-converto/compare/v0.5.1...v0.5.2) (2026-04-08)


### Bug Fixes

* simplify version management, remove hardcoded version in UI ([cef99a9](https://github.com/DevYukine/rom-converto/commit/cef99a91b50b73ec8491aa7fa9137d2762b5e436))



## [0.5.1](https://github.com/DevYukine/rom-converto/compare/v0.5.0...v0.5.1) (2026-04-08)


### Bug Fixes

* **release:** replace .sh file with .js file for syncing version ([d0454ee](https://github.com/DevYukine/rom-converto/commit/d0454eef8370fe2f6b1c8e03575c7911d08e6aff))
* **version:** update all versions on release action ([5c62ea6](https://github.com/DevYukine/rom-converto/commit/5c62ea6042cf6f39225a4e03d7d486a707fd4710))



# [0.5.0](https://github.com/DevYukine/rom-converto/compare/v0.4.0...v0.5.0) (2026-04-08)


### Bug Fixes

* **dev:** force reloading on dev server ([e7d7ba0](https://github.com/DevYukine/rom-converto/commit/e7d7ba0057a55d891b78d95b66b82dc59e7f1b88))
* icons need to be RGBA ([dc9cd89](https://github.com/DevYukine/rom-converto/commit/dc9cd895d1a0f9b93dc7fff0f1378e3345bb73c1))


### Features

* **cdn-to-cia:** add compress flag ([c70017d](https://github.com/DevYukine/rom-converto/commit/c70017d2e6462530aafeb0042253b40b384013f9))
* **compression:** add proper progress tracking ([0b9f7d2](https://github.com/DevYukine/rom-converto/commit/0b9f7d20b12dcc8712707678f879958c5abc977e))
* **gui:** add auto scroll to lists ([f0d4ea9](https://github.com/DevYukine/rom-converto/commit/f0d4ea9fe8cf98045cf89764f6fcff616502394e))
* **gui:** add gui ([9b145e5](https://github.com/DevYukine/rom-converto/commit/9b145e52e1c86ca34d6d38530f9625a66e5e9857))
* **gui:** add loading screen ([509f167](https://github.com/DevYukine/rom-converto/commit/509f167f9611aad1ff01c209d326d89dad3156f2))
* **gui:** force lock decrypt flag when compress is enabled ([7e27522](https://github.com/DevYukine/rom-converto/commit/7e27522d6e68d66e2f12a8536b053f3a0ba327e0))
* **gui:** improve progress tracking & drag & drop for files ([3e91091](https://github.com/DevYukine/rom-converto/commit/3e91091f454f1471ba6350f54bca677d53391b61))
* **gui:** improve UI for bigger screens ([5ab4773](https://github.com/DevYukine/rom-converto/commit/5ab4773d5ad309ff481eec7ab50f0ee5e46bc754))
* **progress:** add a total progress bar for recursive ([0b8c9eb](https://github.com/DevYukine/rom-converto/commit/0b8c9eb49581bc09fd1dd4403c51b1c00f140d64))



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



