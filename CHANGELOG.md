# [0.9.0](https://github.com/DevYukine/rom-converto/compare/v0.8.0...v0.9.0) (2026-05-21)


### Bug Fixes

* **ctr:** preserve CIA meta section through decrypt ([ed38ffb](https://github.com/DevYukine/rom-converto/commit/ed38ffb8cd9c5e21af3a72b68674034f20a5e3e6))
* **gui:** keep verify options visible in batch mode ([8660371](https://github.com/DevYukine/rom-converto/commit/8660371a066667fd9b9e41091243713a8be025bb))
* **nx:** only decode .ncz entries during NSZ verify ([8504822](https://github.com/DevYukine/rom-converto/commit/85048227f93372ffadf33183fa45fc47dd5ebd44))
* **nx:** use embedded tickets when verifying NSZ NCAs ([962ce5d](https://github.com/DevYukine/rom-converto/commit/962ce5d9dc68b9494211fd10d2b42b3fb434e138))


### Features

* **ctr:** add --recursive batch mode to decrypt, compress, decompress, verify ([6f2cb52](https://github.com/DevYukine/rom-converto/commit/6f2cb52389f13738d180bca58e2cfee4998b9b42))
* **ctr:** add conversion between CIA and CCI/3DS ([ee93595](https://github.com/DevYukine/rom-converto/commit/ee93595c0e2955a791519dc3e43a5789adde764b))
* **ctr:** default decrypt output to <name>.decrypted.<ext> ([b37e84b](https://github.com/DevYukine/rom-converto/commit/b37e84b01b3b003af3ea4e2d2e19df948b017eac))
* **gui:** add batch mode to Switch verify ([f642b4f](https://github.com/DevYukine/rom-converto/commit/f642b4ffdac2cbe6cf252b5328497a4b0f2fd9c0))
* **gui:** add Convert ROM link to 3DS sidebar ([4edccd9](https://github.com/DevYukine/rom-converto/commit/4edccd917948b5cdc627f5827dcff3b513e11ac3))
* **gui:** add Switch section to sidebar ([acd543b](https://github.com/DevYukine/rom-converto/commit/acd543b51cc0d1d0c68ed21d3507d6d8a80858e0))
* **nx:** add Switch NSP/XCI compression and decompression ([e102df8](https://github.com/DevYukine/rom-converto/commit/e102df865eee8653303e54f4324069af8503ccf3))



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



