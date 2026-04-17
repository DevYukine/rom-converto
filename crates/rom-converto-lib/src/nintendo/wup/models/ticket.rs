//! Wii U ticket (`title.tik`) parser.
//!
//! The Wii U ticket header is a 0x220-byte fixed-layout structure.
//! Field offsets are taken verbatim from `ETicketFileHeaderWiiU` in
//! Cemu's `src/Cemu/ncrypto/ncrypto.cpp`. We use hand-rolled
//! offset reads rather than binrw because we only need a handful of
//! fields and the ticket layout has a lot of padding that binrw
//! wouldn't clean up.

use crate::nintendo::wup::error::{WupError, WupResult};

/// Fixed base size of a Wii U ticket header. Optional V1 extensions
/// (AOC content rights) sit beyond this offset but we don't parse
/// them in v1.
pub const WUP_TICKET_BASE_SIZE: usize = 0x220;

/// Expected ticket format version ("v1" Wii U ticket) that Cemu's
/// parser tolerates. Older v0 Wii tickets aren't used for Wii U
/// content, so we accept either 0 or 1 without distinguishing.
pub const WUP_TICKET_FORMAT_V1: u8 = 1;

const OFFSET_SIGNATURE_TYPE: usize = 0x000;
const OFFSET_TICKET_FORMAT_VERSION: usize = 0x1BC;
const OFFSET_ENCRYPTED_TITLE_KEY: usize = 0x1BF;
const OFFSET_TICKET_ID: usize = 0x1D0;
const OFFSET_DEVICE_ID: usize = 0x1D8;
const OFFSET_TITLE_ID: usize = 0x1DC;
const OFFSET_TITLE_VERSION: usize = 0x1E6;
const OFFSET_ACCOUNT_ID: usize = 0x21C;

/// Minimal parsed view over a Wii U `title.tik`. Contains just the
/// fields we need to derive the title key and identify the title;
/// everything else in the ticket header is ignored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WupTicket {
    pub signature_type: u32,
    pub ticket_format_version: u8,
    pub encrypted_title_key: [u8; 16],
    pub ticket_id: u64,
    pub device_id: u32,
    pub title_id: u64,
    pub title_version: u16,
    pub account_id: u32,
}

impl WupTicket {
    /// Parse a Wii U ticket from a byte slice. Fails with
    /// [`WupError::InvalidTicket`] if the slice is shorter than the
    /// base header size.
    pub fn parse(bytes: &[u8]) -> WupResult<Self> {
        if bytes.len() < WUP_TICKET_BASE_SIZE {
            return Err(WupError::InvalidTicket);
        }
        Ok(Self {
            signature_type: read_u32_be(bytes, OFFSET_SIGNATURE_TYPE),
            ticket_format_version: bytes[OFFSET_TICKET_FORMAT_VERSION],
            encrypted_title_key: bytes[OFFSET_ENCRYPTED_TITLE_KEY..OFFSET_ENCRYPTED_TITLE_KEY + 16]
                .try_into()
                .unwrap(),
            ticket_id: read_u64_be(bytes, OFFSET_TICKET_ID),
            device_id: read_u32_be(bytes, OFFSET_DEVICE_ID),
            title_id: read_u64_be(bytes, OFFSET_TITLE_ID),
            title_version: read_u16_be(bytes, OFFSET_TITLE_VERSION),
            account_id: read_u32_be(bytes, OFFSET_ACCOUNT_ID),
        })
    }

    /// True if the ticket is bound to a specific device (as opposed
    /// to a generic retail download). Personalised tickets need an
    /// extra depersonalisation step before the title key can be
    /// decrypted with the common key, which v1 does not support.
    pub fn is_personalized(&self) -> bool {
        self.device_id != 0
    }
}

fn read_u16_be(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32_be(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

fn read_u64_be(bytes: &[u8], offset: usize) -> u64 {
    u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_ticket(title_id: u64, key: [u8; 16]) -> Vec<u8> {
        let mut ticket = vec![0u8; WUP_TICKET_BASE_SIZE];
        // signature type: RSA-2048 SHA256 (0x00010004).
        ticket[0..4].copy_from_slice(&0x0001_0004u32.to_be_bytes());
        ticket[OFFSET_TICKET_FORMAT_VERSION] = WUP_TICKET_FORMAT_V1;
        ticket[OFFSET_ENCRYPTED_TITLE_KEY..OFFSET_ENCRYPTED_TITLE_KEY + 16].copy_from_slice(&key);
        // Deterministic ticket id / account id so the tests have
        // something concrete to assert against.
        ticket[OFFSET_TICKET_ID..OFFSET_TICKET_ID + 8]
            .copy_from_slice(&0x1234_5678_ABCD_EF01u64.to_be_bytes());
        ticket[OFFSET_DEVICE_ID..OFFSET_DEVICE_ID + 4].copy_from_slice(&0u32.to_be_bytes());
        ticket[OFFSET_TITLE_ID..OFFSET_TITLE_ID + 8].copy_from_slice(&title_id.to_be_bytes());
        ticket[OFFSET_TITLE_VERSION..OFFSET_TITLE_VERSION + 2]
            .copy_from_slice(&32u16.to_be_bytes());
        ticket[OFFSET_ACCOUNT_ID..OFFSET_ACCOUNT_ID + 4]
            .copy_from_slice(&0x0000_0042u32.to_be_bytes());
        ticket
    }

    #[test]
    fn parses_all_fields() {
        let key = [0x11u8; 16];
        let bytes = make_ticket(0x0005_000E_1010_2000, key);
        let ticket = WupTicket::parse(&bytes).unwrap();
        assert_eq!(ticket.signature_type, 0x0001_0004);
        assert_eq!(ticket.ticket_format_version, WUP_TICKET_FORMAT_V1);
        assert_eq!(ticket.encrypted_title_key, key);
        assert_eq!(ticket.ticket_id, 0x1234_5678_ABCD_EF01);
        assert_eq!(ticket.device_id, 0);
        assert_eq!(ticket.title_id, 0x0005_000E_1010_2000);
        assert_eq!(ticket.title_version, 32);
        assert_eq!(ticket.account_id, 0x42);
    }

    #[test]
    fn is_personalized_only_when_device_id_set() {
        let mut bytes = make_ticket(0x0005_000E_1010_2000, [0u8; 16]);
        let ticket = WupTicket::parse(&bytes).unwrap();
        assert!(!ticket.is_personalized());

        bytes[OFFSET_DEVICE_ID..OFFSET_DEVICE_ID + 4]
            .copy_from_slice(&0x1234_5678u32.to_be_bytes());
        let ticket = WupTicket::parse(&bytes).unwrap();
        assert!(ticket.is_personalized());
        assert_eq!(ticket.device_id, 0x1234_5678);
    }

    #[test]
    fn rejects_short_buffer() {
        let short = vec![0u8; WUP_TICKET_BASE_SIZE - 1];
        let err = WupTicket::parse(&short);
        assert!(matches!(err, Err(WupError::InvalidTicket)));
    }

    #[test]
    fn parses_minimum_sized_buffer() {
        let bytes = make_ticket(0x0005_000E_0000_0001, [0u8; 16]);
        assert_eq!(bytes.len(), WUP_TICKET_BASE_SIZE);
        let ticket = WupTicket::parse(&bytes).unwrap();
        assert_eq!(ticket.title_id, 0x0005_000E_0000_0001);
    }
}
