# [0.19.0](https://github.com/DevYukine/rom-converto/compare/v0.18.0...v0.19.0) (2026-09-01)


### Bug Fixes

* **chd:** chdman-parity track padding, per-track cht2, datasizes; sanitize icon filename stems ([8ef7f5d](https://github.com/DevYukine/rom-converto/commit/8ef7f5de7d2664ed748f60e51e3ee6ac991ba676))
* **gui:** pin tauri plugin crates to npm package minors ([756c313](https://github.com/DevYukine/rom-converto/commit/756c313015cc1a859f6a1e6c5c3685e637f4b4f1))
* **gui:** update wup tooltips for optional disc keys ([53867ea](https://github.com/DevYukine/rom-converto/commit/53867ea4afe5d4f6fd98c9fa90f481d0bfe95cde))


### Features

* **cue:** batch convert folders of cue/bin discs in cli and gui ([492416f](https://github.com/DevYukine/rom-converto/commit/492416f7aab3ca6bec03722719887c9f0bebd6fb))
* **gui:** add tooltips to every option control ([73b59f9](https://github.com/DevYukine/rom-converto/commit/73b59f923e2cd1a272e585891906b2451431904c))
* **info:** read ps1, ps2, and psp metadata with auto-detect info command ([5de8fb7](https://github.com/DevYukine/rom-converto/commit/5de8fb7b202486b6d0b3cae7cbfe51c873d29508))
* **nx:** document default prod.keys paths and color the gui keys row by found status ([7ddda5b](https://github.com/DevYukine/rom-converto/commit/7ddda5b125cab978121905146b04f5611e72d8f1))
* **ps3:** decrypt encrypted ISOs and extract disc metadata ([071a8bd](https://github.com/DevYukine/rom-converto/commit/071a8bdfc17f7e7588ce28928d87512fa013e908))
* **ps3:** decrypt with built-in disc keys, make --key optional ([a0c2d12](https://github.com/DevYukine/rom-converto/commit/a0c2d126a49397a704697ab5258083184ca529ae))
* **wup:** embed disc key database and make disc key optional ([37451fe](https://github.com/DevYukine/rom-converto/commit/37451fed80afdaeec4434fdc10cd1d33e11396d6))
* **xbox:** read game metadata for xbox and xbox 360 info command ([7cb783f](https://github.com/DevYukine/rom-converto/commit/7cb783fa7a46a289c90c91d107faead50608c7fa))
* **xbox:** support original xbox xiso and xbox 360 zar conversion ([84c2a52](https://github.com/DevYukine/rom-converto/commit/84c2a52fd0e356e09701d991c72915968d5ec359))



# [0.18.0](https://github.com/DevYukine/rom-converto/compare/v0.17.0...v0.18.0) (2026-08-31)


### Bug Fixes

* **ctr:** resolve cdn-to-cia failures with large tmds, key patching, and missing optional contents ([47666fb](https://github.com/DevYukine/rom-converto/commit/47666fb75137e813d37ea876be971e24780f61fa))
* expand tilde paths and create missing output parent dirs ([657de6d](https://github.com/DevYukine/rom-converto/commit/657de6dec1472ee27ac628fc7420a8dd69aa9bb9))
* **gui:** expand dropped folders before staging in dat verify ([a70c3db](https://github.com/DevYukine/rom-converto/commit/a70c3db649af55a16def6002fd1da3cfc84355a7))
* **gui:** keep folder drops from being staged as files ([abd743f](https://github.com/DevYukine/rom-converto/commit/abd743f232baa49252576e5040c7d1278eca38c5))
* **gui:** sort dropped files by name ([b9a7e5a](https://github.com/DevYukine/rom-converto/commit/b9a7e5acc016e8af70e084b171c64b7afa203c2e))
* **lint:** resolve clippy errors in chd huffman and extract worker ([d4de224](https://github.com/DevYukine/rom-converto/commit/d4de2244462aa0baeeffa7bf9f383c4e50185d92))


### Features

* **chd:** support all chdman codecs with selectable codec sets and levels ([b3f8cc7](https://github.com/DevYukine/rom-converto/commit/b3f8cc7999ce6985d23f0ed98fcaf631c9bd44cb))
* **ctr:** support DSiWare/TWL cias and verify forged cdn ticket keys ([b3c4212](https://github.com/DevYukine/rom-converto/commit/b3c4212fe90571f327f49fd394d3b6ea425370b8))
* **gui:** add right-click copy and text selection to DAT results ([6db18a3](https://github.com/DevYukine/rom-converto/commit/6db18a333037be84eb8d8302b79ad9c76abb72d6))



# [0.17.0](https://github.com/DevYukine/rom-converto/compare/v0.16.0...v0.17.0) (2026-07-16)


### Bug Fixes

* **archive:** support additional input codecs ([eb5df98](https://github.com/DevYukine/rom-converto/commit/eb5df983a5815a66a4835ec30fed58e4d5cb448a))
* **ci:** drop unsupported musl FFI build ([81a130f](https://github.com/DevYukine/rom-converto/commit/81a130f36f66d16137c06c6df3e5ffcd4835c4be))
* **ci:** generate nuxt types on install so gui tests can resolve tsconfig ([467f18b](https://github.com/DevYukine/rom-converto/commit/467f18b7ca31e886eaa3395f176578d06b976dd4))
* **ci:** pin linuxdeploy for Linux AppImages ([e36bfcc](https://github.com/DevYukine/rom-converto/commit/e36bfcc476bde43dca8703783c05f9e8c5efa633))
* **ctr:** harden CDN output publication ([fe3696f](https://github.com/DevYukine/rom-converto/commit/fe3696ffc5d3d1b41169f7a61e26175839d931b7))
* **ctr:** preserve CDN conversion outputs ([0bf6290](https://github.com/DevYukine/rom-converto/commit/0bf62902792d5c0a3b94ad12758b2b255c0a07c0))
* **gui:** keep dots in output filenames when deriving from archives ([cb25d14](https://github.com/DevYukine/rom-converto/commit/cb25d142b7d4c1175786c3a1ddc8b043a19d663f))
* **gui:** parse WUP metadata response ([4df3250](https://github.com/DevYukine/rom-converto/commit/4df3250c8b8e037350098b8b7321cc6948ba1877))


### Features

* **ffi:** add C ABI integration ([9464c15](https://github.com/DevYukine/rom-converto/commit/9464c15bd6cab26d89594681b1e35e05bb0fa728))
* **gui:** add desktop updater ([bfec539](https://github.com/DevYukine/rom-converto/commit/bfec5394a8413967d3bab8ce0b10e518cef71ca0))
* **gui:** redesign interface with operation-first layout and global queue ([040c371](https://github.com/DevYukine/rom-converto/commit/040c371e16edf139f9a15350e336ef62fb976dda))



# [0.16.0](https://github.com/DevYukine/rom-converto/compare/v0.15.0...v0.16.0) (2026-07-11)


### Bug Fixes

* **ci:** avoid stale gui target cache ([bbd28e0](https://github.com/DevYukine/rom-converto/commit/bbd28e071f09825d65129d88c50db4e7e9a6a8f9))
* **ci:** mirror rar platform fixes in release builds ([b5ec2b7](https://github.com/DevYukine/rom-converto/commit/b5ec2b78a0ebf04036b950462497fef8d7fb58f4))
* **ci:** stabilize linux gui bundle build ([ef4709d](https://github.com/DevYukine/rom-converto/commit/ef4709de317045df43f764f1d273e210395e3847))
* **cli:** make updater match release asset names ([a1eb1e7](https://github.com/DevYukine/rom-converto/commit/a1eb1e7fefe3c7685db3338df5778b26af88a4db))
* **gui:** add tauri desktop icons ([02cdfa9](https://github.com/DevYukine/rom-converto/commit/02cdfa9217f73d3016ef7bfecc3f053e1918ff22))
* **nx:** stub content-bearing non-secure partitions on xci compress ([e84e593](https://github.com/DevYukine/rom-converto/commit/e84e5934d2a0b751ff94b5a72f9707f976cec247))


### Features

* **benchmark:** add rom-converto-benchmark crate ([0db037a](https://github.com/DevYukine/rom-converto/commit/0db037af7a21bfe2a2a93ea9322d379793298cbe))
* **ci:** add linux arm64 gui builds ([7a6462f](https://github.com/DevYukine/rom-converto/commit/7a6462f8fbf3d447d089158bb3e10bfbe4f9c571))
* **cue:** add cue to iso and cso/zso conversion ([da4f16f](https://github.com/DevYukine/rom-converto/commit/da4f16fa8333855b79eee8ff902e92f629f7fbc7))



# [0.15.0](https://github.com/DevYukine/rom-converto/compare/v0.14.0...v0.15.0) (2026-07-07)


### Bug Fixes

* **gui:** keep DAT scan rows across navigation ([f095140](https://github.com/DevYukine/rom-converto/commit/f0951408e58976d8a88b0a2ecadb8ad2aeac1c70))


### Features

* accept zip, 7z, rar, and tar archive inputs transparently ([9363db3](https://github.com/DevYukine/rom-converto/commit/9363db35b69e902b15e90b5f2ae7e73f9a7fdd82))
* **cli:** add overall batch progress bar with size and eta ([a4b2114](https://github.com/DevYukine/rom-converto/commit/a4b2114af8bd9babc573e7f3da905ec655e89f46))
* **cli:** tiered checksum policy for dat verify and scan ([8e3f0e6](https://github.com/DevYukine/rom-converto/commit/8e3f0e6c916b16426209d120a81b278ae57de2db))
* **ctr:** add encrypt command ([95f450a](https://github.com/DevYukine/rom-converto/commit/95f450a57a706e9a0dce220140dc674d6887e77a))
* **dat:** show matched DAT file ([eec0881](https://github.com/DevYukine/rom-converto/commit/eec08813b277eb59202f74c7e4e7b8bb4efef220))
* **gui:** add CTR encrypt page ([5027c0b](https://github.com/DevYukine/rom-converto/commit/5027c0b6c4ee19246dc1d73794c43197a043ac96))
* **gui:** cancellable dat scan with live outcome rows ([77ceaa6](https://github.com/DevYukine/rom-converto/commit/77ceaa6c9e0ef1073029626172f5914a6e7ef050))
* **gui:** managed batch queue with sections, reorder, retry ([0d7a6c2](https://github.com/DevYukine/rom-converto/commit/0d7a6c29eb4bdafb9428a60c3117823dc65f64fd))
* **gui:** named presets backed by the config toml ([36af11a](https://github.com/DevYukine/rom-converto/commit/36af11ae1c2c100afaf0f89580702417252c9ed4))
* **gui:** notify and taskbar progress on batch completion ([95ad1a4](https://github.com/DevYukine/rom-converto/commit/95ad1a4580bc524287381d21397501b597891155))
* **gui:** show before/after comparison card after conversion ([472c126](https://github.com/DevYukine/rom-converto/commit/472c126932666a7ba774c9e41457ea22c210f491))
* one-step cso/zso/dax to chd conversion and reverse ([41a57c6](https://github.com/DevYukine/rom-converto/commit/41a57c69a5f849061ab7b63e52cad5ef00a7cf86))
* persistent content hash and verify cache ([1e903e2](https://github.com/DevYukine/rom-converto/commit/1e903e2089ffeb40d024baf87a172fcde5774710))
* trust zip crc32 for quick dat verify ([37e8b9e](https://github.com/DevYukine/rom-converto/commit/37e8b9e397640065d024630908d66b743d59af4c))
* warn on known format footguns ([4ccc311](https://github.com/DevYukine/rom-converto/commit/4ccc31181803727e3b9f98f812977d86d033adff))



