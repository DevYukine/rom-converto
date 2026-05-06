//! Switch ticket parser. We only need two fields out of a `.tik` for
//! NCA crypto: the 16-byte rights_id (matches the NCA's rights_id at
//! header offset 0x230) and the 16-byte encrypted titlekey, plus the
//! master_key_revision to pick the right `titlekek_xx`. Layout per
//! switchbrew.org/wiki/Ticket and confirmed against
//! `nsz/Fs/Ticket.py`.

use crate::nintendo::nx::error::{NxError, NxResult};

#[derive(Debug, Clone)]
pub struct Ticket {
    pub rights_id: [u8; 16],
    pub encrypted_title_key: [u8; 16],
    pub master_key_revision: u8,
}

impl Ticket {
    pub fn parse(buf: &[u8]) -> NxResult<Self> {
        if buf.len() < 4 {
            return Err(NxError::InvalidTicket);
        }
        let signature_type = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        // Signature type IDs from nsz Type.TicketSignature. Switch
        // retail tickets are typically 0x10004 (RSA-2048-SHA256) at
        // 0x100 bytes, which puts the ticket data at 0x140.
        let sig_size = match signature_type {
            0x10000 | 0x10003 => 0x200,
            0x10001 | 0x10004 => 0x100,
            0x10002 | 0x10005 => 0x3C,
            _ => return Err(NxError::InvalidTicket),
        };
        let after_sig = 4 + sig_size;
        let pad = 0x40 - (after_sig % 0x40);
        let data_off = after_sig + pad;
        if buf.len() < data_off + 0x180 {
            return Err(NxError::InvalidTicket);
        }
        let mut encrypted_title_key = [0u8; 16];
        encrypted_title_key.copy_from_slice(&buf[data_off + 0x40..data_off + 0x50]);
        let master_key_revision = buf[data_off + 0x145];
        let mut rights_id = [0u8; 16];
        rights_id.copy_from_slice(&buf[data_off + 0x160..data_off + 0x170]);
        Ok(Self {
            rights_id,
            encrypted_title_key,
            master_key_revision,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn synth_ticket() -> Vec<u8> {
        // 0x10004 = RSA-2048-SHA256: sig 0x100, after 0x104, pad 0x3C
        // (to round up to 0x140), data_off = 0x140. Total minimum
        // file size = 0x140 + 0x180 = 0x2C0.
        let mut buf = vec![0u8; 0x2C0];
        buf[0..4].copy_from_slice(&0x10004u32.to_le_bytes());
        let data_off = 0x140;
        buf[data_off + 0x40] = 0xAA;
        buf[data_off + 0x4F] = 0xBB;
        buf[data_off + 0x145] = 0x05;
        buf[data_off + 0x160] = 0xCC;
        buf[data_off + 0x16F] = 0xDD;
        buf
    }

    #[test]
    fn parses_rsa2048_sha256_ticket() {
        let buf = synth_ticket();
        let t = Ticket::parse(&buf).unwrap();
        assert_eq!(t.encrypted_title_key[0], 0xAA);
        assert_eq!(t.encrypted_title_key[15], 0xBB);
        assert_eq!(t.master_key_revision, 0x05);
        assert_eq!(t.rights_id[0], 0xCC);
        assert_eq!(t.rights_id[15], 0xDD);
    }

    #[test]
    fn rejects_truncated() {
        let buf = vec![0u8; 4];
        assert!(matches!(Ticket::parse(&buf), Err(NxError::InvalidTicket)));
    }

    #[test]
    fn rejects_unknown_signature() {
        let mut buf = vec![0u8; 0x2C0];
        buf[0..4].copy_from_slice(&0xCAFEBABEu32.to_le_bytes());
        assert!(matches!(Ticket::parse(&buf), Err(NxError::InvalidTicket)));
    }
}
