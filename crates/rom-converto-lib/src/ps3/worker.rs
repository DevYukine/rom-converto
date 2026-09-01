//! Pool worker that decrypts a chunk of PS3 sectors.

use crate::ps3::crypto::decrypt_sector;
use crate::ps3::error::Ps3Error;
use crate::ps3::region::SECTOR_SIZE;
use crate::util::worker_pool::Worker;

pub(crate) struct Ps3DecryptWork {
    pub(crate) start_lba: u32,
    pub(crate) plain: bool,
    pub(crate) bytes: Vec<u8>,
}

pub(crate) struct Ps3DecryptedOut {
    pub(crate) bytes: Vec<u8>,
}

pub(crate) struct Ps3DecryptWorker {
    key: [u8; 16],
}

impl Worker<Ps3DecryptWork, Ps3DecryptedOut, Ps3Error> for Ps3DecryptWorker {
    fn process(&mut self, mut work: Ps3DecryptWork) -> Result<Ps3DecryptedOut, Ps3Error> {
        if !work.plain {
            for (i, sector) in work
                .bytes
                .as_chunks_mut::<SECTOR_SIZE>()
                .0
                .iter_mut()
                .enumerate()
            {
                decrypt_sector(&self.key, work.start_lba + i as u32, sector);
            }
        }
        Ok(Ps3DecryptedOut { bytes: work.bytes })
    }
}

pub(crate) fn make_ps3_decrypt_workers(n: usize, key: [u8; 16]) -> Vec<Ps3DecryptWorker> {
    (0..n).map(|_| Ps3DecryptWorker { key }).collect()
}
