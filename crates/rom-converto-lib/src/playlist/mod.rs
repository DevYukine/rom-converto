//! Multi-disc playlist generation: groups sibling disc image files by title
//! and writes `.m3u` playlists that emulators use to prompt disc swaps.

mod detect;
mod write;

pub use detect::{DiscGroup, group_disc_files, parse_disc_token};
pub use write::{PlaylistMode, PlaylistOptions, PlaylistPlan, plan_playlists};
