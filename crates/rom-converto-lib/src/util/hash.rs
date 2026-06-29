use crate::util::ProgressReporter;
use crc::{CRC_32_ISO_HDLC, Crc};
use sha2::Digest as _;
use std::io::Read;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HashAlgo {
    Crc32,
    Sha1,
    Md5,
    Sha256,
}

impl HashAlgo {
    pub fn label(self) -> &'static str {
        match self {
            HashAlgo::Crc32 => "crc32",
            HashAlgo::Sha1 => "sha1",
            HashAlgo::Md5 => "md5",
            HashAlgo::Sha256 => "sha256",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDigests {
    pub crc32: Option<String>,
    pub sha1: Option<String>,
    pub md5: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: u64,
}

impl FileDigests {
    pub fn value(&self, algo: HashAlgo) -> Option<&str> {
        match algo {
            HashAlgo::Crc32 => self.crc32.as_deref(),
            HashAlgo::Sha1 => self.sha1.as_deref(),
            HashAlgo::Md5 => self.md5.as_deref(),
            HashAlgo::Sha256 => self.sha256.as_deref(),
        }
    }
}

/// Parse a comma-separated `--algo` value into a deduplicated set of
/// algorithms, normalized to a stable column order (crc32, sha1, md5,
/// sha256) regardless of how the user ordered them.
pub fn parse_algos(spec: &str) -> Result<Vec<HashAlgo>, String> {
    let mut crc32 = false;
    let mut sha1 = false;
    let mut md5 = false;
    let mut sha256 = false;
    let mut seen_any = false;
    for token in spec.split(',') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        seen_any = true;
        match token.to_ascii_lowercase().as_str() {
            "crc32" => crc32 = true,
            "sha1" => sha1 = true,
            "md5" => md5 = true,
            "sha256" => sha256 = true,
            other => {
                return Err(format!(
                    "unknown algorithm: {other} (expected crc32, sha1, md5, or sha256)"
                ));
            }
        }
    }
    if !seen_any {
        return Err("no algorithms given (expected crc32, sha1, md5, or sha256)".to_string());
    }
    let mut algos = Vec::new();
    if crc32 {
        algos.push(HashAlgo::Crc32);
    }
    if sha1 {
        algos.push(HashAlgo::Sha1);
    }
    if md5 {
        algos.push(HashAlgo::Md5);
    }
    if sha256 {
        algos.push(HashAlgo::Sha256);
    }
    Ok(algos)
}

/// Compute every requested digest for `path` in a single streaming pass.
/// The file is read in fixed-size chunks and every selected hasher is fed
/// each chunk, so memory stays constant no matter how large the file is.
pub fn hash_file(
    path: &Path,
    algos: &[HashAlgo],
    progress: &dyn ProgressReporter,
) -> std::io::Result<FileDigests> {
    let want = |a: HashAlgo| algos.contains(&a);

    let mut file = std::fs::File::open(path)?;
    let size_bytes = file.metadata()?.len();
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    progress.start(size_bytes, &format!("Hashing {name}"));

    let crc_algo = Crc::<u32>::new(&CRC_32_ISO_HDLC);
    let mut crc = want(HashAlgo::Crc32).then(|| crc_algo.digest());
    let mut sha1 = want(HashAlgo::Sha1).then(sha1::Sha1::new);
    let mut md5 = want(HashAlgo::Md5).then(md_5::Md5::new);
    let mut sha256 = want(HashAlgo::Sha256).then(sha2::Sha256::new);

    let mut buf = vec![0u8; 4 * 1024 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        if let Some(d) = crc.as_mut() {
            d.update(chunk);
        }
        if let Some(h) = sha1.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = md5.as_mut() {
            h.update(chunk);
        }
        if let Some(h) = sha256.as_mut() {
            h.update(chunk);
        }
        progress.inc(n as u64);
    }
    progress.finish();

    Ok(FileDigests {
        crc32: crc.map(|d| format!("{:08x}", d.finalize())),
        sha1: sha1.map(|h| hex::encode(h.finalize())),
        md5: md5.map(|h| hex::encode(h.finalize())),
        sha256: sha256.map(|h| hex::encode(h.finalize())),
        size_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::NoProgress;

    const ALL: &[HashAlgo] = &[
        HashAlgo::Crc32,
        HashAlgo::Sha1,
        HashAlgo::Md5,
        HashAlgo::Sha256,
    ];

    fn hash_bytes(data: &[u8], algos: &[HashAlgo]) -> FileDigests {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("input.bin");
        std::fs::write(&path, data).unwrap();
        hash_file(&path, algos, &NoProgress).unwrap()
    }

    #[test]
    fn empty_input_known_answers() {
        let d = hash_bytes(b"", ALL);
        assert_eq!(d.crc32.as_deref(), Some("00000000"));
        assert_eq!(
            d.sha1.as_deref(),
            Some("da39a3ee5e6b4b0d3255bfef95601890afd80709")
        );
        assert_eq!(d.md5.as_deref(), Some("d41d8cd98f00b204e9800998ecf8427e"));
        assert_eq!(
            d.sha256.as_deref(),
            Some("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
        );
        assert_eq!(d.size_bytes, 0);
    }

    #[test]
    fn abc_known_answers() {
        let d = hash_bytes(b"abc", ALL);
        assert_eq!(d.crc32.as_deref(), Some("352441c2"));
        assert_eq!(
            d.sha1.as_deref(),
            Some("a9993e364706816aba3e25717850c26c9cd0d89d")
        );
        assert_eq!(d.md5.as_deref(), Some("900150983cd24fb0d6963f7d28e17f72"));
        assert_eq!(
            d.sha256.as_deref(),
            Some("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad")
        );
        assert_eq!(d.size_bytes, 3);
    }

    #[test]
    fn single_pass_multi_hash_matches_individual() {
        let data: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let all = hash_bytes(&data, ALL);
        let crc = hash_bytes(&data, &[HashAlgo::Crc32]);
        let sha1 = hash_bytes(&data, &[HashAlgo::Sha1]);
        let md5 = hash_bytes(&data, &[HashAlgo::Md5]);
        let sha256 = hash_bytes(&data, &[HashAlgo::Sha256]);
        assert_eq!(all.crc32, crc.crc32);
        assert_eq!(all.sha1, sha1.sha1);
        assert_eq!(all.md5, md5.md5);
        assert_eq!(all.sha256, sha256.sha256);
    }

    #[test]
    fn requested_algos_only() {
        let d = hash_bytes(b"abc", &[HashAlgo::Sha1]);
        assert!(d.sha1.is_some());
        assert!(d.crc32.is_none());
        assert!(d.md5.is_none());
        assert!(d.sha256.is_none());
    }

    #[test]
    fn hex_widths() {
        let d = hash_bytes(b"abc", ALL);
        assert_eq!(d.crc32.as_ref().unwrap().len(), 8);
        assert_eq!(d.sha1.as_ref().unwrap().len(), 40);
        assert_eq!(d.md5.as_ref().unwrap().len(), 32);
        assert_eq!(d.sha256.as_ref().unwrap().len(), 64);
        for value in [d.crc32, d.sha1, d.md5, d.sha256].into_iter().flatten() {
            assert!(
                value
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
            );
        }
    }

    #[test]
    fn parse_algos_normalizes_order() {
        assert_eq!(
            parse_algos("sha1,crc32").unwrap(),
            vec![HashAlgo::Crc32, HashAlgo::Sha1]
        );
    }

    #[test]
    fn parse_algos_is_case_insensitive() {
        assert_eq!(parse_algos("SHA256").unwrap(), vec![HashAlgo::Sha256]);
    }

    #[test]
    fn parse_algos_dedupes() {
        assert_eq!(parse_algos("crc32,crc32").unwrap(), vec![HashAlgo::Crc32]);
    }

    #[test]
    fn parse_algos_rejects_unknown() {
        assert!(parse_algos("crc32,sha3").is_err());
    }

    #[test]
    fn parse_algos_rejects_empty() {
        assert!(parse_algos("").is_err());
        assert!(parse_algos(" , ").is_err());
    }
}
