//! Gate for the console key material compiled in with the `embedded-keys`
//! feature. A build without it carries no keys, and every operation that
//! needs one fails with [`NoEmbeddedKeys`].

/// Error for an operation that needs a console key this build was compiled
/// without.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("this build has no embedded console keys (built without the `embedded-keys` feature)")]
pub struct NoEmbeddedKeys;

/// Key material that is only present in builds with the `embedded-keys`
/// feature.
pub type EmbeddedKey<T> = Result<T, NoEmbeddedKeys>;

/// Evaluates to `Ok(key)` with the `embedded-keys` feature and to
/// `Err(NoEmbeddedKeys)` without it. The key expression is not compiled
/// into a keyless build.
macro_rules! embedded_key {
    ($key:expr) => {{
        #[cfg(feature = "embedded-keys")]
        {
            Ok($key)
        }
        #[cfg(not(feature = "embedded-keys"))]
        {
            Err($crate::util::embedded_keys::NoEmbeddedKeys)
        }
    }};
}

pub(crate) use embedded_key;

#[cfg(all(test, not(feature = "embedded-keys")))]
mod tests {
    use super::NoEmbeddedKeys;
    use crate::nintendo::ctr::title_key::generate_title_key;
    use crate::nintendo::disc::rvz::error::RvzError;
    use crate::nintendo::ntr::{SECURE_BLOCK_LEN, crypt_secure_area};
    use crate::nintendo::rvl::constants::WII_TICKET_SIZE;
    use crate::nintendo::wup::error::WupError;

    #[test]
    fn key_users_fail_with_no_embedded_keys() {
        assert!(matches!(
            crate::nintendo::rvl::disc::decrypt_title_key(&[0; WII_TICKET_SIZE]),
            Err(RvzError::NoEmbeddedKeys(_))
        ));
        assert_eq!(
            generate_title_key("0004000000030800", None)
                .unwrap_err()
                .to_string(),
            NoEmbeddedKeys.to_string()
        );
        assert!(matches!(
            crate::nintendo::wup::crypto::decrypt_title_key(&[0; 16], 0),
            Err(WupError::NoEmbeddedKeys(_))
        ));
        assert_eq!(
            crate::nintendo::wup::title_key_derive::derive_title_key(0),
            Err(NoEmbeddedKeys)
        );
        assert_eq!(
            crypt_secure_area(&mut [0; SECURE_BLOCK_LEN], 0, true),
            Err(NoEmbeddedKeys)
        );
        assert_eq!(crate::sony::psp::amctrl::bb_mac(&[]), Err(NoEmbeddedKeys));
    }

    #[test]
    fn disc_key_databases_are_empty() {
        assert!(crate::nintendo::wup::disc::embedded_keys::embedded_keys().is_empty());
        assert_eq!(
            crate::sony::ps3::embedded_keys::embedded_key("BLUS30490"),
            None
        );
    }
}
