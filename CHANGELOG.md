# [0.7.0](https://github.com/DevYukine/rom-converto/compare/v0.6.0...v0.7.0) (2026-04-15)


### Bug Fixes

* **ctr:** correct TMD signature and content hash verification ([7b59ece](https://github.com/DevYukine/rom-converto/commit/7b59ecef8294a6c71df58e476f2390967a724261))


### Features

* **ctr:** distinguish decrypted CIAs in Standard classification ([eb3033b](https://github.com/DevYukine/rom-converto/commit/eb3033b5d7b1f15e6c6bf76cf96f038703f364c3))
* **gui:** redesign navigation with per-console pages and collapsible sidebar ([695623f](https://github.com/DevYukine/rom-converto/commit/695623f9ebbec4e292868f4fbc40b4fbec712364))
* **rvz:** add GameCube and Wii disc image compression ([95cc486](https://github.com/DevYukine/rom-converto/commit/95cc486721d216362440f5e3783ce0dcafb3ef3c))


### Performance Improvements

* **chd:** parallel compress, extract, and verify pipelines ([943ca08](https://github.com/DevYukine/rom-converto/commit/943ca08e90f879010806c26c9881fa0465595f5c))



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



