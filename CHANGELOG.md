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



## [0.5.1](https://github.com/DevYukine/rom-converto/compare/v0.5.0...v0.5.1) (2026-04-08)


### Bug Fixes

* **release:** replace .sh file with .js file for syncing version ([d0454ee](https://github.com/DevYukine/rom-converto/commit/d0454eef8370fe2f6b1c8e03575c7911d08e6aff))
* **version:** update all versions on release action ([5c62ea6](https://github.com/DevYukine/rom-converto/commit/5c62ea6042cf6f39225a4e03d7d486a707fd4710))



