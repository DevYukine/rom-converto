# [0.14.0](https://github.com/DevYukine/rom-converto/compare/v0.13.0...v0.14.0) (2026-07-03)


### Bug Fixes

* **cli:** apply config and preset level and chunk size to dol and rvl migrate ([42a11ac](https://github.com/DevYukine/rom-converto/commit/42a11acc0400cd27b6217275d56547bfe43611b9))
* **gui:** enforce pnpm minimum release age and re-resolve electron-to-chromium ([48bec8e](https://github.com/DevYukine/rom-converto/commit/48bec8e0dad1fa849ede53caa9c61635fa872f80))


### Features

* **cli:** gate dol and rvl verify by container console with legacy inputs documented ([e8be15e](https://github.com/DevYukine/rom-converto/commit/e8be15e770f4a827986ddd1d25d55cae20ced574))
* **dat:** playmatch-backed verify, scan, rename, identify and fixdat (CLI + GUI) ([7ff04b0](https://github.com/DevYukine/rom-converto/commit/7ff04b00df3d35923ddbd6790c3971ed0910df06))
* **nintendo:** migrate legacy GCZ, WIA, and NKit disc images to RVZ ([6e3ed7b](https://github.com/DevYukine/rom-converto/commit/6e3ed7b765c390ab35acf98e1970b68ff547634e))



# [0.13.0](https://github.com/DevYukine/rom-converto/compare/v0.12.0...v0.13.0) (2026-06-30)


### Bug Fixes

* **cli:** apply --on-conflict to recursive cdn-to-cia ([56acf56](https://github.com/DevYukine/rom-converto/commit/56acf5634be6cfcfa28c62930e83b8dbb0a9578d))
* **gui:** clear stale page error when starting a batch run ([157a1a4](https://github.com/DevYukine/rom-converto/commit/157a1a4641ee8a02f381ac3efd4bbeee018070d8))
* **gui:** make hash run cancellable ([f99ec0d](https://github.com/DevYukine/rom-converto/commit/f99ec0d80031d3773396ec1b6b6e226183701fa3))
* **gui:** move run output above the operation card ([dbc001f](https://github.com/DevYukine/rom-converto/commit/dbc001f80569e5f99b036e3373897a9d769dfe6a))
* **gui:** reset stale run status and messages on a new run ([af0f251](https://github.com/DevYukine/rom-converto/commit/af0f2515c0dfb5f85d205a03c77cdfb885c8b9e6))
* **gui:** treat decrypted and compressed 3DS files as passing in ctr verify ([5bc5448](https://github.com/DevYukine/rom-converto/commit/5bc544848c83fdfb6c872756cc7fbf6e4518b33d))
* **gui:** truncate long filenames with a middle ellipsis in FileDropZone ([548f6a4](https://github.com/DevYukine/rom-converto/commit/548f6a4b609c62db29e6c66fd40cc257909b2456))
* **lib:** detect encrypted CIA via TMD content-chunk flags in compress pre-flight ([c65db54](https://github.com/DevYukine/rom-converto/commit/c65db5499a8f5022cd500d96246fea72e05064da))
* **lib:** skip OS and NAS junk files and dirs during scans ([5adf7b8](https://github.com/DevYukine/rom-converto/commit/5adf7b84295609eb01598da048a3b0167dafe7ff))
* **lib:** support compressed Z3DS files in ctr info and console detection ([689e189](https://github.com/DevYukine/rom-converto/commit/689e189254a6dab528243c1ac542c4cc49629a7e))


### Features

* add disk headroom preflight before writes ([3da863c](https://github.com/DevYukine/rom-converto/commit/3da863c5d6717e120164d10e14b0114e61f43f7e))
* cancel running operations ([38ce143](https://github.com/DevYukine/rom-converto/commit/38ce1436d2872a821cfc96707508af1d9362084a))
* **cli:** add --dry-run ([809969e](https://github.com/DevYukine/rom-converto/commit/809969e3bcf51769067f945ec1a7d2a648bc31b6))
* **cli:** add --on-conflict policy ([307bd15](https://github.com/DevYukine/rom-converto/commit/307bd15cc9d3b6cd5632b870b507010543c6dead))
* **cli:** add --output-template ([af0bc25](https://github.com/DevYukine/rom-converto/commit/af0bc2535cf72e2b37f4c29c398a99e8ed26778b))
* **cli:** add --report (csv, json, html) ([95d4ec9](https://github.com/DevYukine/rom-converto/commit/95d4ec9f7e6e18f598c45199f6c46c85e353d1eb))
* **cli:** add config file with presets ([89fc82a](https://github.com/DevYukine/rom-converto/commit/89fc82aab4b3db0b93774b729e96b50561cfc5cb))
* **cli:** add hash command ([57d0e04](https://github.com/DevYukine/rom-converto/commit/57d0e04b4799c86230a42979955a7d9b6946e155))
* **cli:** add overwrite-invalid conflict mode ([53121cf](https://github.com/DevYukine/rom-converto/commit/53121cf530657c97dfb374459ceb79b7d1ed02ba))
* **cli:** add playlist command ([52c4fe4](https://github.com/DevYukine/rom-converto/commit/52c4fe4b2cee88e0658a5ac0801425b10d5ef926))
* **cli:** add verbosity ladder and --debug-log ([0ed0e0f](https://github.com/DevYukine/rom-converto/commit/0ed0e0f709914e9bf91e346620d5529bbe19e59a))
* **cli:** make -R recurse, add --max-depth ([f242c1f](https://github.com/DevYukine/rom-converto/commit/f242c1f3f06ea7a257de748b352f61e14a62d3eb))
* **cli:** show space saved after a run ([e8f1eff](https://github.com/DevYukine/rom-converto/commit/e8f1eff4f36dd7da1a4aa301cfd4b3ecb93d486a))
* **ctr:** refuse to compress encrypted 3DS ROMs ([cfdef87](https://github.com/DevYukine/rom-converto/commit/cfdef87722790ea52b943918eaac39634da44f7f))
* **gui:** add disk-headroom preflight and skip-space-check toggle ([02bba40](https://github.com/DevYukine/rom-converto/commit/02bba4011f6d4df3d60ae62f979c7a4165af5ce9))
* **gui:** add dry-run preview backed by shared lib plan logic ([7d6139e](https://github.com/DevYukine/rom-converto/commit/7d6139e8e48eaf91e56f0f2b9dd05ab613c6cc1c))
* **gui:** add hash and playlist pages and conflict-policy controls ([7c5363f](https://github.com/DevYukine/rom-converto/commit/7c5363fac9a289c595f9c819e07a21fce376c2e0))
* **gui:** add overwrite-invalid conflict mode and recursive folder batch ([bc34b33](https://github.com/DevYukine/rom-converto/commit/bc34b33cc5f6215aac6f8f5a8190c6bcdf4ae4f0))
* **gui:** add run-report export and output-path templating ([e588b1d](https://github.com/DevYukine/rom-converto/commit/e588b1d0e4d8b3afa41e4def8c378ad5fcbb03e3))
* **gui:** default output next to source ([30aa545](https://github.com/DevYukine/rom-converto/commit/30aa5457784c53e36346320afbf4340c3a2a8817))
* **gui:** echo the equivalent CLI command ([fe013b1](https://github.com/DevYukine/rom-converto/commit/fe013b1f6244af76e44d5da3bca50357e5331b92))
* **gui:** gate run button and disable incompatible options with tooltips ([7e1e486](https://github.com/DevYukine/rom-converto/commit/7e1e486904f52e4d17a6fcc2ac87bc9410300829))
* **gui:** show queue count on run button ([9d7c4b4](https://github.com/DevYukine/rom-converto/commit/9d7c4b408347377459c85f43c63f7b64a78fd694))
* **lib:** add cancellable hash_file variant ([a58d7bc](https://github.com/DevYukine/rom-converto/commit/a58d7bcc2eed9c419d4b0c0a0e090e31e7da3999))
* show dev-<hash> version for local builds ([3418c8e](https://github.com/DevYukine/rom-converto/commit/3418c8ea1de03adb97f44aeea4e2e7eaa26f46f7))
* show progress phase labels ([d96b477](https://github.com/DevYukine/rom-converto/commit/d96b477b83b7be531d67b23268a5d1379584f100))



# [0.12.0](https://github.com/DevYukine/rom-converto/compare/v0.11.0...v0.12.0) (2026-06-26)


### Bug Fixes

* **ctr:** align .ncch scratch filename casing so cia decrypt works on case-sensitive filesystems ([fc9b1a1](https://github.com/DevYukine/rom-converto/commit/fc9b1a1d4693f128b1a2f126cb0f15f2f11f7bb5))


### Features

* unified output directory, larger default window, and GUI/CLI UX polish ([f361caa](https://github.com/DevYukine/rom-converto/commit/f361caa520537a4190bfd09445bc4fea74b87347))



# [0.11.0](https://github.com/DevYukine/rom-converto/compare/v0.10.0...v0.11.0) (2026-06-12)


### Features

* **chd:** add dvd-mode chd for ps2/psp and cso/zso support ([c0a0195](https://github.com/DevYukine/rom-converto/commit/c0a019588b77fcb42c4a6adcabc9bee834352a3a))
* **chd:** route cd-media isos to cd-mode chd with chdman parity ([763f1c5](https://github.com/DevYukine/rom-converto/commit/763f1c56c6d9d76949353a8b4e676a140a7459af))
* **ux:** consistent cli flags with --force/--output/--recursive everywhere, verify exit codes, gui verify pages and full cli option parity ([69fe088](https://github.com/DevYukine/rom-converto/commit/69fe0881e988432642f09869eff9af796d1b465c))
* **verify:** disc integrity for dol/rvl/wup, chd extract/verify --recursive, ctr seed-crypto info, wup WUD/WUX info ([d6c3564](https://github.com/DevYukine/rom-converto/commit/d6c3564fffc519f5f03de85cce646f2e628369ca))



# [0.10.0](https://github.com/DevYukine/rom-converto/compare/v0.9.1...v0.10.0) (2026-06-09)


### Bug Fixes

* **ctr:** embed retail cert chain and ticket content index for converted CIAs ([eacbdf0](https://github.com/DevYukine/rom-converto/commit/eacbdf07b201ea0de9d5c733a4a6a3da1dd0d152))


### Features

* **cue:** merge multi-bin disc images into a single bin/cue ([3775d40](https://github.com/DevYukine/rom-converto/commit/3775d40255767ee40d01eb61ff46798381ad4d91))
* **info:** cross-console ROM inspection (CLI + GUI) ([99dcad4](https://github.com/DevYukine/rom-converto/commit/99dcad4686c63a8c1b5585ff0f7bc545f910041a))
* **rvl:** add wbfs compression/decompression support ([767f929](https://github.com/DevYukine/rom-converto/commit/767f9296e7658ca9a98bcfdf20bce99d3db4c6c9))



