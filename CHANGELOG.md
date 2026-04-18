# [0.8.0](https://github.com/DevYukine/rom-converto/compare/v0.7.1...v0.8.0) (2026-04-18)


### Bug Fixes

* **gui:** disable nuxt telemetry to unblock tauri dev startup ([fe25fe5](https://github.com/DevYukine/rom-converto/commit/fe25fe5b9537083362d7d4ddbf9bfc46c2c6c90b))
* replace panicking unwraps in updater and CHD GUI commands ([a3db4ba](https://github.com/DevYukine/rom-converto/commit/a3db4ba6046b0e29872817850a2bea156a9d8305))


### Features

* **wup:** add Wii U .wua archive creation ([fd6fbe5](https://github.com/DevYukine/rom-converto/commit/fd6fbe58831c5685ed812627f3271da760436557))



## [0.7.1](https://github.com/DevYukine/rom-converto/compare/v0.7.0...v0.7.1) (2026-04-15)


### Bug Fixes

* **chd:** match chdman compression byte-for-byte via zlib backend ([a19bf00](https://github.com/DevYukine/rom-converto/commit/a19bf007f3475d576987a4db96c197d1ca81a833))


### Performance Improvements

* **ctr:** parallel Z3DS compress/decompress and configurable level ([b38601c](https://github.com/DevYukine/rom-converto/commit/b38601c140cbd3df1c1cce40d62c552bbd7a8898))



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



