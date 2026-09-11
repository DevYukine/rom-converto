//! PS5 `.pkg` packages: a `\x7FCNT` container, bare or wrapped in a
//! `\x7FFIH`/`\x7FLIH` image, with its metadata in `param.json`.

pub mod pkg;

pub use pkg::{Ps5PkgImage, Ps5PkgInfo};
