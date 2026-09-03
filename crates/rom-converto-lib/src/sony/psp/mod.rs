//! PSP PBP/EBOOT container support: header parsing, `PARAM.SFO` metadata,
//! and plain segment extraction.
//!
//! `DATA.PSAR` is identified but not decrypted: `NPUMDIMG` images stay
//! encrypted on the way out, and the PS1 Classic `PSISOIMG`/`PSTITLEIMG`
//! families are not converted.

pub mod extract;
pub mod info;
pub mod pbp;

pub use extract::extract_segments;
pub use info::{PbpInfo, PbpSegmentInfo, PsarKind, read_info};
pub use pbp::Pbp;
