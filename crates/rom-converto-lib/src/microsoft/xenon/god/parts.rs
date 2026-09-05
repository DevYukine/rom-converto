//! Write a GoD container's `DataNNNN` part files and their SHA-1 hash
//! tables.
//!
//! A part file is a master hash list block, then up to
//! [`SUBPARTS_PER_PART`] repetitions of `sub hash list block + its
//! [`SUBPART_SIZE`] of data`. A sub hash list holds the SHA-1 of each
//! data block it covers, a master hash list the SHA-1 of each sub hash
//! list block. Once every part exists they are chained backward: part
//! `n`'s master hash list is hashed into part `n - 1`'s, so part 0's
//! digest alone covers the whole container.
//!
//! Hashing 204 blocks per subpart is CPU-bound and independent per
//! subpart, so it runs on a [`crate::util::worker_pool::Pool`] and comes
//! back in sequence order for the sequential writer.

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use sha1::{Digest, Sha1};

use crate::util::CancelToken;
use crate::util::worker_pool::{Pool, Worker, drive, parallelism};

use super::error::{GodError, GodResult};
use super::layout::{BLOCK_SIZE, BLOCKS_PER_PART_FILE, GodScan, SUBPART_SIZE, SUBPARTS_PER_PART};

/// Bytes per SHA-1 entry in a hash list.
const DIGEST_SIZE: usize = 20;

/// Outcome of writing a GoD container's part files: the MHT root hash
/// chaining them together, plus their combined size on disk.
pub struct PartsSummary {
    /// SHA-1 of part 0's master hash list, the root of the whole chain.
    pub mht_root: [u8; 20],
    pub total_size: u64,
}

fn part_path(dir: &Path, index: u64) -> PathBuf {
    dir.join(format!("Data{index:04}"))
}

struct SubpartHashWorker;

impl Worker<Vec<u8>, (Vec<u8>, Vec<u8>), GodError> for SubpartHashWorker {
    fn process(&mut self, data: Vec<u8>) -> GodResult<(Vec<u8>, Vec<u8>)> {
        let mut list = vec![0u8; BLOCK_SIZE as usize];
        for (index, block) in data.chunks(BLOCK_SIZE as usize).enumerate() {
            list[index * DIGEST_SIZE..(index + 1) * DIGEST_SIZE]
                .copy_from_slice(&Sha1::digest(block));
        }
        Ok((list, data))
    }
}

/// Sequential writer over the part files: opens each part on its first
/// subpart, accumulates that part's master hash list in memory, and
/// backpatches it at offset 0 once the part is full.
struct PartsWriter {
    dir: PathBuf,
    file: Option<std::fs::File>,
    master: Vec<u8>,
    /// Subparts already written into the open part.
    filled: usize,
    part_index: u64,
    /// Size of the part file closed most recently.
    last_size: u64,
}

impl PartsWriter {
    fn new(dir: &Path) -> Self {
        Self {
            dir: dir.to_path_buf(),
            file: None,
            master: vec![0u8; BLOCK_SIZE as usize],
            filled: 0,
            part_index: 0,
            last_size: 0,
        }
    }

    fn push(&mut self, sub_list: &[u8], data: &[u8]) -> GodResult<()> {
        if self.file.is_none() {
            let mut file = std::fs::File::create(part_path(&self.dir, self.part_index))?;
            // The master hash list leads the file but is only known once
            // every subpart behind it has been hashed.
            file.write_all(&self.master)?;
            self.file = Some(file);
        }
        let file = self.file.as_mut().expect("the part file was just opened");
        file.write_all(sub_list)?;
        file.write_all(data)?;

        self.master[self.filled * DIGEST_SIZE..(self.filled + 1) * DIGEST_SIZE]
            .copy_from_slice(&Sha1::digest(sub_list));
        self.filled += 1;
        if self.filled == SUBPARTS_PER_PART as usize {
            self.close()?;
        }
        Ok(())
    }

    fn close(&mut self) -> GodResult<()> {
        let Some(mut file) = self.file.take() else {
            return Ok(());
        };
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&self.master)?;
        self.last_size = file.metadata()?.len();
        self.master.fill(0);
        self.filled = 0;
        self.part_index += 1;
        Ok(())
    }
}

fn read_master(dir: &Path, index: u64) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(part_path(dir, index))?;
    let mut master = vec![0u8; BLOCK_SIZE as usize];
    file.read_exact(&mut master)?;
    Ok(master)
}

/// Chains the parts backward and returns the resulting MHT root: each
/// part's master hash list is hashed into the entry that follows its
/// predecessor's own sub hash list entries.
fn chain_masters(dir: &Path, part_count: u64) -> GodResult<[u8; 20]> {
    let chain_entry = SUBPARTS_PER_PART as usize * DIGEST_SIZE;
    let mut master = read_master(dir, part_count - 1)?;
    for index in (0..part_count - 1).rev() {
        let digest = Sha1::digest(&master);
        let mut previous = read_master(dir, index)?;
        previous[chain_entry..chain_entry + DIGEST_SIZE].copy_from_slice(&digest);
        std::fs::OpenOptions::new()
            .write(true)
            .open(part_path(dir, index))?
            .write_all(&previous)?;
        master = previous;
    }
    Ok(Sha1::digest(&master).into())
}

/// Writes every part file for `scan`'s partition into `dir`.
pub fn write_parts<R: Read + Seek>(
    reader: &mut R,
    scan: &GodScan,
    dir: &Path,
    bytes_done: &AtomicU64,
    cancel: &CancelToken,
) -> GodResult<PartsSummary> {
    let subpart_count = scan.data_size.div_ceil(SUBPART_SIZE);
    let n_threads = parallelism();
    let workers: Vec<SubpartHashWorker> = (0..n_threads).map(|_| SubpartHashWorker).collect();
    let pool: Pool<Vec<u8>, (Vec<u8>, Vec<u8>), GodError> = Pool::spawn(workers);

    let mut writer = PartsWriter::new(dir);
    let result = drive(
        &pool,
        subpart_count,
        n_threads * 2,
        |seq| {
            if cancel.is_cancelled() {
                return Err(GodError::Cancelled);
            }
            let start = seq * SUBPART_SIZE;
            let len = SUBPART_SIZE.min(scan.data_size - start);
            // The trailing block is zero-padded to a full block so every
            // part file stays block-aligned and every sub hash entry
            // covers a whole block.
            let mut data = vec![0u8; len.div_ceil(BLOCK_SIZE) as usize * BLOCK_SIZE as usize];
            reader.seek(SeekFrom::Start(scan.base + start))?;
            reader.read_exact(&mut data[..len as usize])?;
            Ok(data)
        },
        |seq, (sub_list, data)| {
            if cancel.is_cancelled() {
                return Err(GodError::Cancelled);
            }
            writer.push(&sub_list, &data)?;
            bytes_done.fetch_add(
                SUBPART_SIZE.min(scan.data_size - seq * SUBPART_SIZE),
                Ordering::Relaxed,
            );
            Ok(())
        },
    );
    pool.shutdown();
    result?;
    writer.close()?;

    Ok(PartsSummary {
        mht_root: chain_masters(dir, scan.part_count)?,
        total_size: writer.last_size + (scan.part_count - 1) * BLOCK_SIZE * BLOCKS_PER_PART_FILE,
    })
}

#[cfg(test)]
mod tests {
    use super::super::xex::ExecutionId;
    use super::*;
    use std::io::Cursor;

    fn scan_for(data_size: u64) -> GodScan {
        GodScan {
            base: 0,
            data_size,
            block_count: data_size.div_ceil(BLOCK_SIZE),
            part_count: 1,
            title_name: None,
            execution: ExecutionId {
                media_id: 0,
                title_id: 0,
                platform: 0,
                executable_type: 0,
                disc_number: 0,
                disc_count: 0,
            },
        }
    }

    #[test]
    fn a_short_part_pads_its_tail_block_and_hashes_every_sub_list() {
        // Two full blocks plus a short trailing one, all inside a single
        // subpart, so the part file is master + sub list + data.
        let data_size = 2 * BLOCK_SIZE + 777;
        let data: Vec<u8> = (0..data_size).map(|i| (i % 251) as u8).collect();
        let mut padded = data.clone();
        padded.resize(3 * BLOCK_SIZE as usize, 0);

        let dir = tempfile::tempdir().unwrap();
        let summary = write_parts(
            &mut Cursor::new(data.clone()),
            &scan_for(data_size),
            dir.path(),
            &AtomicU64::new(0),
            &CancelToken::new(),
        )
        .unwrap();

        let part = std::fs::read(dir.path().join("Data0000")).unwrap();
        assert_eq!(part.len() as u64 % BLOCK_SIZE, 0);
        assert_eq!(part.len() as u64, 5 * BLOCK_SIZE);
        assert_eq!(summary.total_size, part.len() as u64);

        let master = &part[..BLOCK_SIZE as usize];
        let sub_list = &part[BLOCK_SIZE as usize..2 * BLOCK_SIZE as usize];
        for (index, block) in padded.chunks(BLOCK_SIZE as usize).enumerate() {
            assert_eq!(
                &sub_list[index * DIGEST_SIZE..(index + 1) * DIGEST_SIZE],
                Sha1::digest(block).as_slice(),
                "block {index}"
            );
        }
        // Blocks past the data are unused and stay zero.
        assert!(sub_list[3 * DIGEST_SIZE..].iter().all(|&b| b == 0));
        assert_eq!(&master[..DIGEST_SIZE], Sha1::digest(sub_list).as_slice());
        assert!(master[DIGEST_SIZE..].iter().all(|&b| b == 0));
        assert_eq!(summary.mht_root.as_slice(), Sha1::digest(master).as_slice());
        assert_eq!(&part[2 * BLOCK_SIZE as usize..], &padded[..]);
    }

    #[test]
    fn a_subpart_boundary_starts_a_second_sub_hash_list() {
        let data_size = SUBPART_SIZE + BLOCK_SIZE;
        let data: Vec<u8> = (0..data_size).map(|i| (i % 251) as u8).collect();

        let dir = tempfile::tempdir().unwrap();
        write_parts(
            &mut Cursor::new(data.clone()),
            &scan_for(data_size),
            dir.path(),
            &AtomicU64::new(0),
            &CancelToken::new(),
        )
        .unwrap();

        let part = std::fs::read(dir.path().join("Data0000")).unwrap();
        let second_list =
            &part[(BLOCK_SIZE + BLOCK_SIZE + SUBPART_SIZE) as usize..][..BLOCK_SIZE as usize];
        assert_eq!(
            &second_list[..DIGEST_SIZE],
            Sha1::digest(&data[SUBPART_SIZE as usize..]).as_slice()
        );
        let master = &part[..BLOCK_SIZE as usize];
        assert_eq!(
            &master[DIGEST_SIZE..2 * DIGEST_SIZE],
            Sha1::digest(second_list).as_slice()
        );
        assert_eq!(
            &part[2 * BLOCK_SIZE as usize..][..SUBPART_SIZE as usize],
            &data[..SUBPART_SIZE as usize],
            "the first sub hash list precedes the first subpart's data"
        );
    }

    #[test]
    fn chaining_folds_each_part_master_into_its_predecessor() {
        let dir = tempfile::tempdir().unwrap();
        let masters: Vec<Vec<u8>> = (0..3u8)
            .map(|part| {
                let mut master = vec![0u8; BLOCK_SIZE as usize];
                master[..DIGEST_SIZE].fill(part + 1);
                std::fs::write(dir.path().join(format!("Data{part:04}")), &master).unwrap();
                master
            })
            .collect();

        let root = chain_masters(dir.path(), 3).unwrap();

        let chain_entry = SUBPARTS_PER_PART as usize * DIGEST_SIZE;
        let mut expected_one = masters[1].clone();
        expected_one[chain_entry..chain_entry + DIGEST_SIZE]
            .copy_from_slice(&Sha1::digest(&masters[2]));
        let mut expected_zero = masters[0].clone();
        expected_zero[chain_entry..chain_entry + DIGEST_SIZE]
            .copy_from_slice(&Sha1::digest(&expected_one));

        assert_eq!(
            std::fs::read(dir.path().join("Data0001")).unwrap(),
            expected_one
        );
        assert_eq!(
            std::fs::read(dir.path().join("Data0000")).unwrap(),
            expected_zero
        );
        assert_eq!(root.as_slice(), Sha1::digest(&expected_zero).as_slice());
    }

    #[test]
    fn a_cancelled_token_stops_before_any_part_is_finished() {
        let cancel = CancelToken::new();
        cancel.cancel();
        let dir = tempfile::tempdir().unwrap();
        let result = write_parts(
            &mut Cursor::new(vec![0u8; BLOCK_SIZE as usize]),
            &scan_for(BLOCK_SIZE),
            dir.path(),
            &AtomicU64::new(0),
            &cancel,
        );
        assert!(matches!(result, Err(GodError::Cancelled)));
    }
}
