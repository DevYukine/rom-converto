//! XEX2 basefile recovery: AES-128-CBC with an all-zero IV, then the basic
//! or LZX decompression path (xenia `xex_module.cc`).

use aes::{
    Aes128,
    cipher::{BlockModeDecrypt, KeyIvInit},
};
use block_padding::NoPadding;
use lzxd::{Lzxd, WindowSize};
use sha1::{Digest, Sha1};

use super::{Compression, FileFormatInfo, SecurityInfo, read_u16, read_u32};

type Aes128CbcDec = cbc::Decryptor<Aes128>;

/// The 360 is end of life and this key is public, so metadata reads work
/// without the user supplying anything.
pub(super) const RETAIL_KEY: [u8; 16] = [
    0x20, 0xB1, 0x85, 0xA5, 0x9D, 0x28, 0xFD, 0xC3, 0x40, 0x58, 0x3F, 0xBB, 0x08, 0x96, 0xBF, 0x91,
];
const DEVKIT_KEY: [u8; 16] = [0u8; 16];

const ENCRYPTION_NONE: u16 = 0;
const ENCRYPTION_NORMAL: u16 = 1;

const BLOCK_INFO_LEN: usize = 24;
const LZX_CHUNK: usize = 32768;
/// Real 360 basefiles are tens of megabytes; this only stops a corrupt
/// `image_size` from asking for a multi-gigabyte allocation.
const MAX_IMAGE_SIZE: usize = 256 * 1024 * 1024;

fn cbc_decrypt_zero_iv(key: &[u8; 16], buf: &mut [u8]) -> Option<()> {
    Aes128CbcDec::new_from_slices(key, &[0u8; 16])
        .ok()?
        .decrypt_padded::<NoPadding>(buf)
        .ok()?;
    Some(())
}

/// Each region is its own CBC stream, so the IV restarts at zero every call.
fn decrypt_region(key: &[u8; 16], buf: &mut [u8]) -> Option<()> {
    let aligned = buf.len() - buf.len() % 16;
    cbc_decrypt_zero_iv(key, &mut buf[..aligned])
}

fn session_key(base_key: &[u8; 16], encrypted: &[u8; 16]) -> Option<[u8; 16]> {
    let mut key = *encrypted;
    cbc_decrypt_zero_iv(base_key, &mut key)?;
    Some(key)
}

fn window_size(raw: u32) -> Option<WindowSize> {
    Some(match raw {
        0x0000_8000 => WindowSize::KB32,
        0x0001_0000 => WindowSize::KB64,
        0x0002_0000 => WindowSize::KB128,
        0x0004_0000 => WindowSize::KB256,
        0x0008_0000 => WindowSize::KB512,
        0x0010_0000 => WindowSize::MB1,
        0x0020_0000 => WindowSize::MB2,
        0x0040_0000 => WindowSize::MB4,
        0x0080_0000 => WindowSize::MB8,
        0x0100_0000 => WindowSize::MB16,
        0x0200_0000 => WindowSize::MB32,
        _ => return None,
    })
}

/// Each block carries the next block's `{size, sha1}` in its first 24 bytes
/// and is hashed whole, so a SHA-1 mismatch on the first block means the
/// session key was wrong rather than the file being corrupt.
fn deblock_lzx(
    buf: &[u8],
    window: WindowSize,
    mut block_size: u32,
    mut block_hash: [u8; 20],
    image_size: usize,
) -> Option<Vec<u8>> {
    let mut lzx = Lzxd::new(window);
    let mut out = Vec::with_capacity(image_size);
    let mut at = 0usize;

    while block_size != 0 {
        let end = at.checked_add(block_size as usize)?;
        let block = buf.get(at..end)?;
        if Sha1::digest(block).as_slice() != block_hash.as_slice() {
            return None;
        }
        let next_size = read_u32(block, 0)?;
        let next_hash = block.get(4..BLOCK_INFO_LEN)?.try_into().ok()?;

        let mut cursor = BLOCK_INFO_LEN;
        while cursor < block.len() {
            let chunk_size = read_u16(block, cursor)? as usize;
            cursor += 2;
            if chunk_size == 0 {
                break;
            }
            let chunk = block.get(cursor..cursor.checked_add(chunk_size)?)?;
            if out.len() >= image_size {
                return Some(out);
            }
            let wanted = LZX_CHUNK.min(image_size - out.len());
            out.extend_from_slice(lzx.decompress_next(chunk, wanted).ok()?);
            cursor += chunk_size;
        }

        at = end;
        block_size = next_size;
        block_hash = next_hash;
    }
    Some(out)
}

pub(crate) fn recover_basefile(
    pe_data: &[u8],
    fmt: &FileFormatInfo,
    image_size: u32,
    session_key: &[u8; 16],
) -> Option<Vec<u8>> {
    let image_size = image_size as usize;
    if image_size == 0 || image_size > MAX_IMAGE_SIZE {
        return None;
    }
    let encrypted = match fmt.encryption_type {
        ENCRYPTION_NONE => false,
        ENCRYPTION_NORMAL => true,
        _ => return None,
    };

    let mut out = match &fmt.compression {
        Compression::None => {
            // Decrypt the whole region, then let the final resize keep
            // `image_size` bytes; truncating first would drop the last
            // partial AES block when `image_size` is not 16-aligned.
            let mut buf = pe_data.to_vec();
            if encrypted {
                decrypt_region(session_key, &mut buf)?;
            }
            buf
        }
        Compression::Basic(blocks) => {
            // The stored (`data_size`) regions are one continuous CBC stream;
            // xenia carries a single IV across every block and the zero runs
            // exist only in the output. Decrypt the concatenated ciphertext
            // once, then scatter the zero gaps.
            let total_data = blocks
                .iter()
                .try_fold(0usize, |acc, &(d, _)| acc.checked_add(d as usize))?;
            let mut stream = pe_data.get(..total_data)?.to_vec();
            if encrypted {
                decrypt_region(session_key, &mut stream)?;
            }
            let mut buf = Vec::new();
            let mut at = 0usize;
            for &(data_size, zero_size) in blocks {
                let end = at.checked_add(data_size as usize)?;
                let filled = buf
                    .len()
                    .checked_add(data_size as usize)?
                    .checked_add(zero_size as usize)?;
                if filled > MAX_IMAGE_SIZE {
                    return None;
                }
                buf.extend_from_slice(stream.get(at..end)?);
                buf.resize(filled, 0);
                at = end;
            }
            buf
        }
        Compression::Normal {
            window_size: raw,
            first_block_size,
            first_block_hash,
        } => {
            let window = window_size(*raw)?;
            let mut buf = pe_data.to_vec();
            if encrypted {
                decrypt_region(session_key, &mut buf)?;
            }
            deblock_lzx(
                &buf,
                window,
                *first_block_size,
                *first_block_hash,
                image_size,
            )?
        }
    };

    out.resize(image_size, 0);
    Some(out)
}

pub(crate) fn decrypt_and_decompress(
    pe_data: &[u8],
    fmt: &FileFormatInfo,
    security: &SecurityInfo,
) -> Option<Vec<u8>> {
    let retail = session_key(&RETAIL_KEY, &security.aes_key)?;
    let out = recover_basefile(pe_data, fmt, security.image_size, &retail);
    if out.is_some() || !matches!(fmt.compression, Compression::Normal { .. }) {
        return out;
    }
    // LZX is the only layout with per-block hashes, so it is the only one that
    // can tell a wrong key from a corrupt file. Anywhere else, guessing devkit
    // would silently hand back garbage.
    let devkit = session_key(&DEVKIT_KEY, &security.aes_key)?;
    recover_basefile(pe_data, fmt, security.image_size, &devkit)
}

#[cfg(test)]
pub(super) fn cbc_encrypt_zero_iv(key: &[u8; 16], buf: &mut [u8]) {
    use aes::cipher::BlockModeEncrypt;

    let len = buf.len();
    cbc::Encryptor::<Aes128>::new_from_slices(key, &[0u8; 16])
        .expect("key and iv are both 16 bytes")
        .encrypt_padded::<NoPadding>(buf, len)
        .expect("buffer length is a multiple of the block size");
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION_KEY: [u8; 16] = [0xA5; 16];

    fn security(aes_key: [u8; 16], image_size: u32) -> SecurityInfo {
        SecurityInfo {
            image_size,
            load_address: 0x8200_0000,
            aes_key,
            region: 0xFF,
            allowed_media: 0,
        }
    }

    #[test]
    fn session_key_is_one_cbc_block_under_the_base_key() {
        let mut encrypted = SESSION_KEY;
        cbc_encrypt_zero_iv(&RETAIL_KEY, &mut encrypted);
        assert_ne!(encrypted, SESSION_KEY);
        assert_eq!(
            session_key(&RETAIL_KEY, &encrypted).expect("valid key"),
            SESSION_KEY
        );
    }

    #[test]
    fn normal_encryption_no_compression_round_trip() {
        let plain: Vec<u8> = (0..256u32).map(|i| (i * 7) as u8).collect();
        let mut pe_data = plain.clone();
        cbc_encrypt_zero_iv(&SESSION_KEY, &mut pe_data);
        assert_ne!(pe_data, plain);

        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NORMAL,
            compression: Compression::None,
        };
        let out = recover_basefile(&pe_data, &fmt, plain.len() as u32, &SESSION_KEY)
            .expect("round trip succeeds");
        assert_eq!(out, plain);
    }

    #[test]
    fn no_compression_pads_and_truncates_to_image_size() {
        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NONE,
            compression: Compression::None,
        };
        let pe_data = vec![0xEE; 64];
        let padded = recover_basefile(&pe_data, &fmt, 96, &SESSION_KEY).expect("padded");
        assert_eq!(padded.len(), 96);
        assert_eq!(&padded[..64], &pe_data[..]);
        assert!(padded[64..].iter().all(|&b| b == 0));

        let cut = recover_basefile(&pe_data, &fmt, 32, &SESSION_KEY).expect("truncated");
        assert_eq!(cut, vec![0xEE; 32]);
    }

    #[test]
    fn basic_compression_round_trip_with_zero_run() {
        // The two stored regions are one continuous CBC stream (xenia chains
        // the IV across the block boundary), so encrypt them together. The
        // zero run belongs only in the decoded output.
        let first = vec![0x11u8; 32];
        let second = vec![0x22u8; 16];
        let mut stream = first.clone();
        stream.extend_from_slice(&second);
        cbc_encrypt_zero_iv(&SESSION_KEY, &mut stream);

        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NORMAL,
            compression: Compression::Basic(vec![(32, 16), (16, 0)]),
        };
        let out = recover_basefile(&stream, &fmt, 64, &SESSION_KEY).expect("round trip succeeds");

        let mut expected = first;
        expected.extend_from_slice(&[0u8; 16]);
        expected.extend_from_slice(&second);
        assert_eq!(out, expected);
    }

    #[test]
    fn unknown_encryption_is_rejected() {
        let fmt = FileFormatInfo {
            encryption_type: 7,
            compression: Compression::None,
        };
        assert!(recover_basefile(&[0u8; 16], &fmt, 16, &SESSION_KEY).is_none());
    }

    #[test]
    fn unknown_window_size_is_rejected() {
        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NONE,
            compression: Compression::Normal {
                window_size: 0x1234,
                first_block_size: 32,
                first_block_hash: [0; 20],
            },
        };
        assert!(recover_basefile(&[0u8; 64], &fmt, 64, &SESSION_KEY).is_none());
    }

    #[test]
    fn lzx_block_hash_mismatch_falls_through_both_keys() {
        // Block framing only: a wrong hash rejects the block before any LZX
        // payload is touched, which is exactly the wrong-key signal.
        let pe_data = vec![0x5Au8; 64];
        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NONE,
            compression: Compression::Normal {
                window_size: 0x0000_8000,
                first_block_size: 64,
                first_block_hash: [0xFF; 20],
            },
        };
        assert!(recover_basefile(&pe_data, &fmt, 64, &SESSION_KEY).is_none());
        assert!(decrypt_and_decompress(&pe_data, &fmt, &security([0; 16], 64)).is_none());
    }

    #[test]
    fn lzx_terminator_block_ends_the_stream() {
        // A first block whose only content is a zero next-block info and a
        // zero chunk prefix: valid framing, no payload.
        let mut block = vec![0u8; 32];
        let hash: [u8; 20] = Sha1::digest(&block).into();
        block.extend_from_slice(&[0u8; 16]);

        let fmt = FileFormatInfo {
            encryption_type: ENCRYPTION_NONE,
            compression: Compression::Normal {
                window_size: 0x0000_8000,
                first_block_size: 32,
                first_block_hash: hash,
            },
        };
        let out = recover_basefile(&block, &fmt, 48, &SESSION_KEY).expect("framing is valid");
        assert_eq!(out, vec![0u8; 48]);
    }
}
