// CD format constants
pub const SECTOR_SIZE: usize = 2352;
pub const SUBCODE_SIZE: usize = 96;
pub const FRAME_SIZE: usize = SECTOR_SIZE + SUBCODE_SIZE;
pub const FRAMES_PER_HUNK: u32 = 8;
pub const CD_HUNK_BYTES: u32 = FRAME_SIZE as u32 * FRAMES_PER_HUNK;

// CD timing
pub const FRAMES_PER_SECOND: u32 = 75;
pub const SECONDS_PER_MINUTE: u32 = 60;

// CD audio
pub const CD_CHANNELS: usize = 2;
pub const BYTES_PER_SAMPLE: usize = 2;
pub const BYTES_PER_STEREO_SAMPLE: usize = CD_CHANNELS * BYTES_PER_SAMPLE;

// I/O
pub const IO_BUFFER_SIZE: usize = 8 * 1024 * 1024;
