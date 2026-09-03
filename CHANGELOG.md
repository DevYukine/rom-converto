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



