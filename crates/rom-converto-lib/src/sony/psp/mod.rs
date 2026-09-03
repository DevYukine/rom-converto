//! PSP PBP/EBOOT container support: header parsing, `PARAM.SFO` metadata,
//! plain segment extraction, and `NPUMDIMG` `DATA.PSAR` to ISO conversion.
//!
//! The PS1 Classic `PSISOIMG`/`PSTITLEIMG` families are identified but not
//! converted.

pub mod amctrl;
pub mod extract;
pub mod info;
pub mod kirk;
pub mod lzrc;
pub mod npumd;
pub mod pbp;

pub use extract::extract_segments;
pub use info::{PbpInfo, PbpSegmentInfo, PsarKind, read_info};
pub use npumd::to_iso;
pub use pbp::Pbp;
