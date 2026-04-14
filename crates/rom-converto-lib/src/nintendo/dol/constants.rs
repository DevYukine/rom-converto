//! GameCube disc constants.

/// Offset of the GameCube magic word inside a disc header (0x1C).
pub const GAMECUBE_MAGIC_OFFSET: usize = 0x1C;

/// GameCube disc magic: `0xC2339F3D`.
pub const GAMECUBE_MAGIC: u32 = 0xC2339F3D;
