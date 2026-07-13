//! Verification of CIA and CCI/3DS ROMs: signature chain validation against
//! the built-in root key and per-partition legitimacy checks.

mod chain;
pub mod root_key;

pub use chain::{
    BatchVerifySummary, CiaLegitimacy, CiaLegitimacySubType, CiaVerifyOptions, CiaVerifyResult,
    CtrVerifyOptions, CtrVerifyResult, NcchPartitionResult, NcsdVerifyResult, StandardSubType,
    verify_cia, verify_cia_cancellable, verify_ctr, verify_ctr_batch, verify_ctr_batch_cancellable,
    verify_ctr_cancellable,
};
