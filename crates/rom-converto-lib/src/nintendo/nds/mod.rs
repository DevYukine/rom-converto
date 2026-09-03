//! Nintendo DS secure-area crypto: turn the KEY1-encrypted 2 KiB block at
//! `0x4000` into plaintext, or put it back.
//!
//! Only the first 0x800 bytes of the secure-area window are encrypted; the
//! rest of the ROM is copied byte-for-byte. The key comes from the header
//! id code plus the embedded KEY1 table (see [`embedded_keys`]), so no key
//! file is ever needed.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use log::info;

use crate::cd::IO_BUFFER_SIZE;
use crate::util::{
    BYTES_PER_MB, CancelToken, ProgressReporter, await_with_progress_cancel, scratch_output_path,
};

pub mod embedded_keys;
pub mod error;
pub mod info;
pub mod key1;

#[cfg(test)]
pub(crate) mod test_fixtures;

pub use error::{NdsError, NdsResult};
pub use key1::Key1;

/// Bytes in the cartridge header.
pub const HEADER_SIZE: usize = 0x200;

/// File offset of the secure-area window.
pub const SECURE_AREA_OFFSET: usize = 0x4000;

/// End of the secure-area window. An ARM9 `rom_offset` at or past this
/// means the title has no secure area.
pub const SECURE_AREA_END: usize = 0x8000;

/// Bytes of the secure area that KEY1 covers.
pub const SECURE_BLOCK_LEN: usize = 0x800;

/// Byte-wise key-code wrap the secure area uses.
const KEYCODE_MODULO: usize = 8;

/// Check value the first block decrypts to.
const SECURE_AREA_ID: &[u8; 8] = b"encryObj";

/// What `ndstool` and melonDS leave at `0x4000` after decrypting: two
/// `0xE7FFDEFF` words, an undefined-instruction pair that doubles as the
/// "already decrypted" marker.
const DECRYPTED_MARKER: &[u8; 8] = &[0xFF, 0xDE, 0xFF, 0xE7, 0xFF, 0xDE, 0xFF, 0xE7];

/// Whether a ROM's secure area holds plaintext or KEY1 ciphertext.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecureAreaState {
    /// The secure area is plaintext.
    Decrypted,
    /// The secure area is KEY1 ciphertext.
    Encrypted,
}

/// Classifies a ROM's secure area from its header and the first 0x800
/// bytes of the secure-area window.
///
/// # Errors
///
/// [`NdsError::NoSecureArea`] when the ARM9 code sits outside the
/// secure-area window or the window is blank, and
/// [`NdsError::SecureAreaCorrupt`] when it is neither plaintext nor
/// decryptable with the key derived from the header id code.
pub fn detect_state(
    header: &[u8; HEADER_SIZE],
    secure: &[u8; SECURE_BLOCK_LEN],
) -> NdsResult<SecureAreaState> {
    let arm9_rom_offset = read_u32(&header[0x20..0x24]) as usize;
    if !(SECURE_AREA_OFFSET..SECURE_AREA_END).contains(&arm9_rom_offset) {
        return Err(NdsError::NoSecureArea);
    }

    let head = &secure[..8];
    if head == DECRYPTED_MARKER || head == SECURE_AREA_ID {
        return Ok(SecureAreaState::Decrypted);
    }

    let idcode = read_u32(&header[0x0C..0x10]);
    let mut block = load_block(secure, 0);
    Key1::new(idcode, 2, KEYCODE_MODULO).decrypt_block(&mut block);
    Key1::new(idcode, 3, KEYCODE_MODULO).decrypt_block(&mut block);
    if block[0].to_le_bytes() == SECURE_AREA_ID[..4]
        && block[1].to_le_bytes() == SECURE_AREA_ID[4..]
    {
        return Ok(SecureAreaState::Encrypted);
    }

    if secure.iter().all(|&b| b == 0x00) || secure.iter().all(|&b| b == 0xFF) {
        return Err(NdsError::NoSecureArea);
    }
    Err(NdsError::SecureAreaCorrupt)
}

/// Encrypts or decrypts the KEY1-covered secure-area block in place.
///
/// The first 64-bit block is crypted twice, at level 2 over level 3, and
/// carries [`SECURE_AREA_ID`] as the check value while encrypted; the
/// remaining 0x7F8 bytes use level 3 only. Decryption leaves
/// [`DECRYPTED_MARKER`] in the first block, matching `ndstool`.
pub fn crypt_secure_area(buf: &mut [u8; SECURE_BLOCK_LEN], idcode: u32, encrypt: bool) {
    let key2 = Key1::new(idcode, 2, KEYCODE_MODULO);
    let key3 = Key1::new(idcode, 3, KEYCODE_MODULO);

    if encrypt {
        for offset in (8..SECURE_BLOCK_LEN).step_by(8) {
            let mut block = load_block(buf, offset);
            key3.encrypt_block(&mut block);
            store_block(buf, offset, block);
        }
        buf[..8].copy_from_slice(SECURE_AREA_ID);
        let mut block = load_block(buf, 0);
        key3.encrypt_block(&mut block);
        key2.encrypt_block(&mut block);
        store_block(buf, 0, block);
    } else {
        let mut block = load_block(buf, 0);
        key2.decrypt_block(&mut block);
        key3.decrypt_block(&mut block);
        store_block(buf, 0, block);
        buf[..8].copy_from_slice(DECRYPTED_MARKER);
        for offset in (8..SECURE_BLOCK_LEN).step_by(8) {
            let mut block = load_block(buf, offset);
            key3.decrypt_block(&mut block);
            store_block(buf, offset, block);
        }
    }
}

/// Default output path for an NDS encrypt: `.encrypted` before the extension.
pub fn derive_encrypted_path(input: &Path) -> PathBuf {
    derive_tagged_path(input, "encrypted")
}

/// Default output path for an NDS decrypt: `.decrypted` before the extension.
pub fn derive_decrypted_path(input: &Path) -> PathBuf {
    derive_tagged_path(input, "decrypted")
}

/// Encrypts a ROM's secure area, copying the rest of the file unchanged.
pub async fn encrypt_nds_rom_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    force: bool,
    cancel: CancelToken,
) -> NdsResult<()> {
    crypt_nds_rom(progress, input_path, output_path, force, cancel, true).await
}

/// Decrypts a ROM's secure area, copying the rest of the file unchanged.
pub async fn decrypt_nds_rom_cancellable(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    force: bool,
    cancel: CancelToken,
) -> NdsResult<()> {
    crypt_nds_rom(progress, input_path, output_path, force, cancel, false).await
}

async fn crypt_nds_rom(
    progress: &dyn ProgressReporter,
    input_path: PathBuf,
    output_path: PathBuf,
    force: bool,
    cancel: CancelToken,
    encrypt: bool,
) -> NdsResult<()> {
    let preexisting = tokio::fs::metadata(&output_path).await.is_ok();
    if preexisting && !force {
        return Err(NdsError::OutputAlreadyExists);
    }

    let input_size = tokio::fs::metadata(&input_path).await?.len();
    if input_size < (SECURE_AREA_OFFSET + SECURE_BLOCK_LEN) as u64 {
        return Err(NdsError::TooSmall);
    }

    let peek_path = input_path.clone();
    let secure = tokio::task::spawn_blocking(move || -> NdsResult<[u8; SECURE_BLOCK_LEN]> {
        let mut file = std::fs::File::open(&peek_path)?;
        let mut header = [0u8; HEADER_SIZE];
        file.read_exact(&mut header)?;
        file.seek(SeekFrom::Start(SECURE_AREA_OFFSET as u64))?;
        let mut secure = [0u8; SECURE_BLOCK_LEN];
        file.read_exact(&mut secure)?;

        match (detect_state(&header, &secure)?, encrypt) {
            (SecureAreaState::Encrypted, true) => return Err(NdsError::AlreadyEncrypted),
            (SecureAreaState::Decrypted, false) => return Err(NdsError::AlreadyDecrypted),
            _ => {}
        }
        crypt_secure_area(&mut secure, read_u32(&header[0x0C..0x10]), encrypt);
        Ok(secure)
    })
    .await??;

    let total_mb = input_size as f64 / BYTES_PER_MB;
    let verb = if encrypt { "Encrypting" } else { "Decrypting" };
    progress.start(
        input_size,
        &format!("{verb} NDS secure area (~{total_mb:.2} MB)"),
    );

    let write_path = scratch_output_path(&output_path)?;
    let input_owned = input_path.clone();
    let write_owned = write_path.to_path_buf();
    let cancel_bg = cancel.clone();
    let bytes_done = Arc::new(AtomicU64::new(0));
    let bytes_done_bg = bytes_done.clone();

    let handle = tokio::task::spawn_blocking(move || -> NdsResult<()> {
        let in_file = std::fs::File::open(&input_owned)?;
        let mut reader = std::io::BufReader::with_capacity(IO_BUFFER_SIZE, in_file);
        let out_file = std::fs::File::create(&write_owned)?;
        let mut writer = std::io::BufWriter::with_capacity(IO_BUFFER_SIZE, out_file);

        let mut chunk = vec![0u8; IO_BUFFER_SIZE];
        loop {
            if cancel_bg.is_cancelled() {
                return Err(NdsError::Cancelled);
            }
            let read = reader.read(&mut chunk)?;
            if read == 0 {
                break;
            }
            writer.write_all(&chunk[..read])?;
            bytes_done_bg.fetch_add(read as u64, Ordering::Relaxed);
        }

        let mut out = writer.into_inner().map_err(|err| err.into_error())?;
        out.seek(SeekFrom::Start(SECURE_AREA_OFFSET as u64))?;
        out.write_all(&secure)?;
        out.flush()?;
        Ok(())
    });

    let cleanup = {
        let write_path = write_path.to_path_buf();
        move || -> NdsError {
            let _ = std::fs::remove_file(&write_path);
            NdsError::Cancelled
        }
    };
    if let Err(err) =
        await_with_progress_cancel(progress, &bytes_done, handle, &cancel, cleanup).await
    {
        let _ = tokio::fs::remove_file(&write_path).await;
        return Err(err);
    }

    crate::util::publish_temp(write_path, &output_path, force)?;

    info!(
        "{} NDS secure area: {:.2} MB from {}",
        if encrypt { "Encrypted" } else { "Decrypted" },
        total_mb,
        input_path.display()
    );
    Ok(())
}

fn derive_tagged_path(input: &Path, tag: &str) -> PathBuf {
    let stem = input.file_stem().and_then(|s| s.to_str()).unwrap_or("out");
    let ext = input.extension().and_then(|s| s.to_str()).unwrap_or("nds");
    input.with_file_name(format!("{stem}.{tag}.{ext}"))
}

fn read_u32(bytes: &[u8]) -> u32 {
    u32::from_le_bytes(bytes.try_into().expect("4-byte slice"))
}

fn load_block(buf: &[u8; SECURE_BLOCK_LEN], offset: usize) -> [u32; 2] {
    [
        read_u32(&buf[offset..offset + 4]),
        read_u32(&buf[offset + 4..offset + 8]),
    ]
}

fn store_block(buf: &mut [u8; SECURE_BLOCK_LEN], offset: usize, block: [u32; 2]) {
    buf[offset..offset + 4].copy_from_slice(&block[0].to_le_bytes());
    buf[offset + 4..offset + 8].copy_from_slice(&block[1].to_le_bytes());
}

#[cfg(test)]
mod tests {
    use super::test_fixtures::{SYNTH_IDCODE, synth_nds};
    use super::*;
    use crate::util::NoProgress;

    fn parts(rom: &[u8]) -> ([u8; HEADER_SIZE], [u8; SECURE_BLOCK_LEN]) {
        let mut header = [0u8; HEADER_SIZE];
        header.copy_from_slice(&rom[..HEADER_SIZE]);
        let mut secure = [0u8; SECURE_BLOCK_LEN];
        secure.copy_from_slice(&rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + SECURE_BLOCK_LEN]);
        (header, secure)
    }

    fn state(rom: &[u8]) -> NdsResult<SecureAreaState> {
        let (header, secure) = parts(rom);
        detect_state(&header, &secure)
    }

    fn idcode() -> u32 {
        u32::from_le_bytes(SYNTH_IDCODE)
    }

    #[test]
    fn fresh_synth_reads_as_decrypted() {
        let rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        assert_eq!(state(&rom).expect("detects"), SecureAreaState::Decrypted);
    }

    #[test]
    fn plain_encryobj_head_reads_as_decrypted() {
        let mut rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + 8].copy_from_slice(SECURE_AREA_ID);
        assert_eq!(state(&rom).expect("detects"), SecureAreaState::Decrypted);
    }

    #[test]
    fn secure_area_round_trips() {
        let rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        let (_, plain) = parts(&rom);

        let mut block = plain;
        crypt_secure_area(&mut block, idcode(), true);
        assert_ne!(block, plain);

        let mut encrypted_rom = rom.clone();
        encrypted_rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + SECURE_BLOCK_LEN]
            .copy_from_slice(&block);
        assert_eq!(
            state(&encrypted_rom).expect("detects"),
            SecureAreaState::Encrypted
        );

        crypt_secure_area(&mut block, idcode(), false);
        assert_eq!(block, plain);
    }

    #[test]
    fn arm9_offset_past_window_has_no_secure_area() {
        let rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_END as u32);
        assert!(matches!(state(&rom), Err(NdsError::NoSecureArea)));
    }

    #[test]
    fn blank_secure_area_has_no_secure_area() {
        let mut rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + SECURE_BLOCK_LEN].fill(0xFF);
        assert!(matches!(state(&rom), Err(NdsError::NoSecureArea)));
    }

    #[test]
    fn all_zero_secure_area_has_no_secure_area() {
        let mut rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + SECURE_BLOCK_LEN].fill(0x00);
        assert!(matches!(state(&rom), Err(NdsError::NoSecureArea)));
    }

    #[test]
    fn garbage_secure_area_is_corrupt() {
        let mut rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + 8].copy_from_slice(b"garbage!");
        assert!(matches!(state(&rom), Err(NdsError::SecureAreaCorrupt)));
    }

    #[test]
    fn key1_block_round_trips_at_both_levels() {
        let plain = [0x0123_4567u32, 0x89AB_CDEFu32];
        let mut level2 = plain;
        let mut level3 = plain;
        let key2 = Key1::new(idcode(), 2, KEYCODE_MODULO);
        let key3 = Key1::new(idcode(), 3, KEYCODE_MODULO);

        key2.encrypt_block(&mut level2);
        key3.encrypt_block(&mut level3);
        assert_ne!(level2, plain);
        assert_ne!(level2, level3);

        key2.decrypt_block(&mut level2);
        key3.decrypt_block(&mut level3);
        assert_eq!(level2, plain);
        assert_eq!(level3, plain);
    }

    async fn write_rom(dir: &Path, name: &str, rom: &[u8]) -> PathBuf {
        let path = dir.join(name);
        tokio::fs::write(&path, rom).await.expect("writes rom");
        path
    }

    #[tokio::test]
    async fn file_round_trip_restores_every_byte() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        let input = write_rom(dir.path(), "game.nds", &rom).await;

        let encrypted = derive_encrypted_path(&input);
        encrypt_nds_rom_cancellable(
            &NoProgress,
            input.clone(),
            encrypted.clone(),
            false,
            CancelToken::new(),
        )
        .await
        .expect("encrypts");

        let encrypted_rom = tokio::fs::read(&encrypted).await.expect("reads");
        assert_eq!(
            state(&encrypted_rom).expect("detects"),
            SecureAreaState::Encrypted
        );

        let decrypted = derive_decrypted_path(&encrypted);
        decrypt_nds_rom_cancellable(
            &NoProgress,
            encrypted.clone(),
            decrypted.clone(),
            false,
            CancelToken::new(),
        )
        .await
        .expect("decrypts");

        assert_eq!(tokio::fs::read(&decrypted).await.expect("reads"), rom);
    }

    #[tokio::test]
    async fn file_round_trip_from_encrypted_restores_every_byte() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        let (_, mut secure) = parts(&rom);
        crypt_secure_area(&mut secure, idcode(), true);
        rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + SECURE_BLOCK_LEN].copy_from_slice(&secure);
        let input = write_rom(dir.path(), "game.nds", &rom).await;

        let decrypted = derive_decrypted_path(&input);
        decrypt_nds_rom_cancellable(
            &NoProgress,
            input.clone(),
            decrypted.clone(),
            false,
            CancelToken::new(),
        )
        .await
        .expect("decrypts");

        let encrypted = derive_encrypted_path(&decrypted);
        encrypt_nds_rom_cancellable(
            &NoProgress,
            decrypted.clone(),
            encrypted.clone(),
            false,
            CancelToken::new(),
        )
        .await
        .expect("encrypts");

        assert_eq!(tokio::fs::read(&encrypted).await.expect("reads"), rom);
    }

    #[tokio::test]
    async fn encrypting_an_encrypted_rom_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        let (_, mut secure) = parts(&rom);
        crypt_secure_area(&mut secure, idcode(), true);
        rom[SECURE_AREA_OFFSET..SECURE_AREA_OFFSET + SECURE_BLOCK_LEN].copy_from_slice(&secure);
        let input = write_rom(dir.path(), "game.nds", &rom).await;

        let err = encrypt_nds_rom_cancellable(
            &NoProgress,
            input.clone(),
            dir.path().join("out.nds"),
            false,
            CancelToken::new(),
        )
        .await
        .expect_err("rejects");
        assert!(matches!(err, NdsError::AlreadyEncrypted));
    }

    #[tokio::test]
    async fn decrypting_a_decrypted_rom_is_rejected() {
        let dir = tempfile::tempdir().expect("tempdir");
        let rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        let input = write_rom(dir.path(), "game.nds", &rom).await;

        let err = decrypt_nds_rom_cancellable(
            &NoProgress,
            input.clone(),
            dir.path().join("out.nds"),
            false,
            CancelToken::new(),
        )
        .await
        .expect_err("rejects");
        assert!(matches!(err, NdsError::AlreadyDecrypted));
    }

    /// Locks the port to `ndstool`. The expected digest comes from running a
    /// direct transcription of `encryption.cpp`'s `encrypt_arm9` over the same
    /// fixture, so a drift in the key schedule fails here rather than silently
    /// producing ROMs no DS will boot.
    #[test]
    fn encrypted_block_matches_ndstool() {
        use sha2::{Digest, Sha256};

        let rom = synth_nds(SYNTH_IDCODE, SECURE_AREA_OFFSET as u32);
        let (_, mut secure) = parts(&rom);
        crypt_secure_area(&mut secure, idcode(), true);
        assert_eq!(
            format!("{:x}", Sha256::digest(secure)),
            "86d31bc47af8118fc8fa92b7b7a3156dccc211fbb875a3c7b63ec8a307c83082"
        );
    }

    #[test]
    fn derived_paths_tag_before_the_extension() {
        assert_eq!(
            derive_encrypted_path(Path::new("/roms/Game.nds")),
            PathBuf::from("/roms/Game.encrypted.nds")
        );
        assert_eq!(
            derive_decrypted_path(Path::new("/roms/Game.nds")),
            PathBuf::from("/roms/Game.decrypted.nds")
        );
    }
}
