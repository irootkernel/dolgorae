use sha2::{Digest, Sha256};

pub mod public_v1 {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/dolgorae.public.v1.rs"));
}

pub const PUBLIC_V1_DESCRIPTOR: &[u8] =
    include_bytes!("../docs/protocol/dolgorae-public-v1.descriptor.pb");
pub const PUBLIC_V1_DESCRIPTOR_SHA256: &str =
    "c29b70f6d1bfca5447ddfc396cb62a9ff4f3bbd10532518af78e4726b7de1252";

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
