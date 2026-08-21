use sha2::{Digest, Sha256};

pub mod public_v1 {
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/dolgorae.public.v1.rs"));
}

pub const PUBLIC_V1_DESCRIPTOR: &[u8] =
    include_bytes!("../docs/protocol/dolgorae-public-v1.descriptor.pb");
pub const PUBLIC_V1_DESCRIPTOR_SHA256: &str =
    "22e605dddc26c145ab6c682955fa4bfcf078b8356d38e94982e118b948965318";

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
