use sha2::{Digest, Sha256};

pub mod public_v1 {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/dolgorae.public.v1.rs"));
}

pub const PUBLIC_V1_DESCRIPTOR: &[u8] =
    include_bytes!("../docs/protocol/dolgorae-public-v1.descriptor.pb");
pub const PUBLIC_V1_DESCRIPTOR_SHA256: &str =
    "3ed39e10057eab70e7eaa18a63254268069cb5bec076d4322c99457436c892ca";

#[must_use]
pub fn public_v1_descriptor_digest() -> String {
    format!("{:x}", Sha256::digest(PUBLIC_V1_DESCRIPTOR))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checked_descriptor_digest_matches_contract() {
        assert_eq!(public_v1_descriptor_digest(), PUBLIC_V1_DESCRIPTOR_SHA256);
    }

    #[test]
    fn generated_public_types_are_available() {
        let _ = public_v1::GetCapabilitiesRequest::default();
    }
}
