//! MAME's CD-FLAC (`cdfl`) codec support: gates the FLAC trial to audio
//! sectors, since running it against data sectors never wins.

/// The Mode-1 sync pattern at the start of a CD data sector. Used to
/// gate the CDFL codec trial: FLAC only ever wins on audio sectors, so
/// running it against data sectors is an expensive no-op.
pub(crate) const CD_SYNC_HEADER: [u8; 12] = [
    0x00, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x00,
];
