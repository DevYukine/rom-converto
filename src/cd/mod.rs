pub mod ecc;

pub const SECTOR_SIZE: usize = 2352;
pub const SUBCODE_SIZE: usize = 96;
pub const FRAME_SIZE: usize = SECTOR_SIZE + SUBCODE_SIZE;
pub const FRAMES_PER_HUNK: u32 = 8;
pub const CD_HUNK_BYTES: u32 = FRAME_SIZE as u32 * FRAMES_PER_HUNK;
