mod detect;
mod write;

pub use detect::{DiscGroup, group_disc_files, parse_disc_token};
pub use write::{PlaylistMode, PlaylistOptions, PlaylistPlan, plan_playlists};

pub const DEFAULT_DISC_EXTS: &[&str] = &["cue", "chd", "iso", "cso", "zso"];
