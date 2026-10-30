//! VAPID (RFC 8292). The private scalar signs the JWT. It is not the message key.

use std::time::{SystemTime, UNIX_EPOCH};

use aws_lc_rs::agreement;
use aws_lc_rs::rand::{SecureRandom, SystemRandom};
#[cfg(test)]
use aws_lc_rs::signature::UnparsedPublicKey;
use aws_lc_rs::signature::{self, EcdsaKeyPair};

use super::b64;

const TWELVE_HOURS: u64 = 12 * 60 * 60;

pub struct VapidMaterial {
    public_b64: String,
    public: Vec<u8>,
    private: Vec<u8>,
    subject: String,
}

impl std::fmt::Debug for VapidMaterial {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("VapidMaterial")
            .field("public_len", &self.public.len())
            .field("private_len", &self.private.len())
            .finish()
    }
}

impl VapidMaterial {
    pub fn public_key(&self) -> &str {
        &self.public_b64
    }

    pub(crate) fn private_scalar(&self) -> &[u8] {
        &self.private
    }

    pub(crate) fn authorization(&self, origin: &str, now: u64) -> Result<String, String> {
        let jwt = sign_jwt(&self.private, &self.public, &self.subject, origin, now)?;
        Ok(format!("vapid t={jwt}, k={}", self.public_b64))
    }
}

pub(crate) fn from_parts(
    public_b64: &str,
    private_b64: &str,
    subject: &str,
) -> Result<VapidMaterial, String> {
    let subject = require_subject(subject)?;
    let private = decode_private(private_b64)?;
    let public = decode_public(public_b64)?;
    require_match(&private, &public)?;
    Ok(VapidMaterial {
        public_b64: b64::encode(&public),
        public,
        private,
        subject,
    })
}

/// `Ok(None)` when all three variables are unset. A partial set is an error that names them.
pub(crate) fn load_optional() -> Result<Option<VapidMaterial>, String> {
    load_from(
        env_nonempty("LIBERADO_VAPID_PUBLIC_KEY"),
        env_nonempty("LIBERADO_VAPID_PRIVATE_KEY"),
        env_nonempty("LIBERADO_VAPID_SUBJECT"),
    )
}

pub(crate) fn load_from(
    public: Option<String>,
    private: Option<String>,
    subject: Option<String>,
) -> Result<Option<VapidMaterial>, String> {
    if public.is_none() && private.is_none() && subject.is_none() {
        return Ok(None);
    }
    from_values(public, private, subject).map(Some)
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn from_values(
    public: Option<String>,
    private: Option<String>,
    subject: Option<String>,
) -> Result<VapidMaterial, String> {
    match (public, private, subject) {
        (Some(public), Some(private), Some(subject)) => from_parts(&public, &private, &subject),
        _ => Err(missing_keys()),
    }
}

fn missing_keys() -> String {
    "Web Push is off. Set LIBERADO_VAPID_PUBLIC_KEY, LIBERADO_VAPID_PRIVATE_KEY, and LIBERADO_VAPID_SUBJECT.".into()
}

fn require_subject(subject: &str) -> Result<String, String> {
    let subject = subject.trim();
    if subject.starts_with("mailto:") && subject.len() > "mailto:".len() {
        return Ok(subject.to_string());
    }
    Err("LIBERADO_VAPID_SUBJECT must be a mailto: URI".into())
}

fn decode_private(text: &str) -> Result<Vec<u8>, String> {
    let bytes = b64::decode(text).map_err(|_| private_length())?;
    if bytes.len() == 32 {
        return Ok(bytes);
    }
    Err(private_length())
}

fn private_length() -> String {
    "LIBERADO_VAPID_PRIVATE_KEY is not a 32-byte P-256 scalar".into()
}

fn decode_public(text: &str) -> Result<Vec<u8>, String> {
    let bytes = b64::decode(text).map_err(|_| public_length())?;
    if bytes.len() == 65 && bytes.first() == Some(&0x04) {
        return Ok(bytes);
    }
    Err(public_length())
}

fn public_length() -> String {
    "LIBERADO_VAPID_PUBLIC_KEY is not a 65-byte P-256 key".into()
}

fn require_match(private: &[u8], public: &[u8]) -> Result<(), String> {
    let derived = public_of(private)?;
    if derived == public {
        return Ok(());
    }
    Err(mismatch())
}

fn mismatch() -> String {
    "LIBERADO_VAPID_PUBLIC_KEY does not match LIBERADO_VAPID_PRIVATE_KEY".into()
}

fn public_of(private: &[u8]) -> Result<Vec<u8>, String> {
    let key = agreement::PrivateKey::from_private_key(&agreement::ECDH_P256, private)
        .map_err(|_| mismatch())?;
    let public = key.compute_public_key().map_err(|_| mismatch())?;
    Ok(public.as_ref().to_vec())
}

fn sign_jwt(
    private: &[u8],
    public: &[u8],
    subject: &str,
    origin: &str,
    now: u64,
) -> Result<String, String> {
    let signing = format!(
        "{}.{}",
        b64::encode(b"{\"typ\":\"JWT\",\"alg\":\"ES256\"}"),
        b64::encode(claims(origin, subject, now.saturating_add(TWELVE_HOURS)).as_bytes())
    );
    let pair = EcdsaKeyPair::from_private_key_and_public_key(
        &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        private,
        public,
    )
    .map_err(|_| mismatch())?;
    let rng = SystemRandom::new();
    let sig = pair
        .sign(&rng, signing.as_bytes())
        .map_err(|_| "VAPID signature failed".to_string())?;
    Ok(format!("{signing}.{}", b64::encode(sig.as_ref())))
}

fn claims(origin: &str, subject: &str, exp: u64) -> String {
    format!("{{\"aud\":\"{origin}\",\"exp\":{exp},\"sub\":\"{subject}\"}}")
}

pub fn vapid_env_lines() -> Result<String, String> {
    let (public, private) = generate_keypair()?;
    Ok(format!(
        "LIBERADO_VAPID_PUBLIC_KEY={public}\nLIBERADO_VAPID_PRIVATE_KEY={private}\n"
    ))
}

fn generate_keypair() -> Result<(String, String), String> {
    for _ in 0..8 {
        if let Some(pair) = one_keypair() {
            return Ok(pair);
        }
    }
    Err("could not generate a P-256 key".into())
}

fn one_keypair() -> Option<(String, String)> {
    let mut scalar = [0u8; 32];
    SystemRandom::new().fill(&mut scalar).ok()?;
    let private = agreement::PrivateKey::from_private_key(&agreement::ECDH_P256, &scalar).ok()?;
    let public = private.compute_public_key().ok()?;
    let bytes = public.as_ref();
    EcdsaKeyPair::from_private_key_and_public_key(
        &signature::ECDSA_P256_SHA256_FIXED_SIGNING,
        &scalar,
        bytes,
    )
    .ok()?;
    Some((b64::encode(bytes), b64::encode(&scalar)))
}

pub(crate) fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
pub(crate) fn verify_jwt(public: &[u8], jwt: &str) -> Result<(), String> {
    let (signing, sig) = split_jwt(jwt)?;
    let signature = b64::decode(sig).map_err(|_| "bad signature".to_string())?;
    let key = UnparsedPublicKey::new(&signature::ECDSA_P256_SHA256_FIXED, public);
    key.verify(signing.as_bytes(), &signature)
        .map_err(|_| "VAPID signature did not verify".to_string())
}

#[cfg(test)]
fn split_jwt(jwt: &str) -> Result<(&str, &str), String> {
    let split_at = jwt.rfind('.').ok_or_else(|| "bad jwt".to_string())?;
    let (signing, sig) = jwt.split_at(split_at);
    Ok((signing, &sig[1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_key_lengths_match_and_the_jwt_verifies() {
        let lines = vapid_env_lines().expect("generate");
        let public = field(&lines, "LIBERADO_VAPID_PUBLIC_KEY=");
        let private = field(&lines, "LIBERADO_VAPID_PRIVATE_KEY=");
        let public_bytes = b64::decode(&public).expect("public");
        let private_bytes = b64::decode(&private).expect("private");
        assert_eq!(public_bytes.len(), 65);
        assert_eq!(public_bytes.first().copied(), Some(0x04));
        assert_eq!(private_bytes.len(), 32);
        let material = from_parts(&public, &private, "mailto:push@example.com").expect("pair");
        let header = material
            .authorization("https://push.example.test", 1_700_000_000)
            .expect("jwt");
        assert!(header.starts_with("vapid t="), "prefix");
        assert!(header.contains(", k="), "k");
        let jwt = header
            .trim_start_matches("vapid t=")
            .split(", k=")
            .next()
            .expect("jwt");
        verify_jwt(&public_bytes, jwt).expect("verify");
    }

    #[test]
    fn bad_parts_name_the_variable_and_not_the_value() {
        let missing = from_values(None, None, None).expect_err("missing");
        assert!(missing.contains("LIBERADO_VAPID_PUBLIC_KEY"), "{missing}");
        assert!(missing.contains("LIBERADO_VAPID_PRIVATE_KEY"), "{missing}");
        let bad_public = from_parts("aaaa", "bbbb", "mailto:a@example.com").expect_err("public");
        assert!(bad_public.contains("LIBERADO_VAPID_"), "{bad_public}");
        assert!(!bad_public.contains("aaaa"), "{bad_public}");
        assert!(!bad_public.contains("bbbb"), "{bad_public}");
        let subject = from_values(
            Some("AAAA".into()),
            Some("AAAA".into()),
            Some("https://example.com".into()),
        )
        .expect_err("subject");
        assert!(subject.contains("mailto:"), "{subject}");
        assert!(!subject.contains("https://example.com"), "{subject}");
    }

    #[test]
    fn unset_keys_are_absent_and_a_partial_set_is_an_error() {
        let absent = load_from(None, None, None).expect("absent");
        assert!(absent.is_none());
        let partial = load_from(Some("AAAA".into()), None, None).expect_err("partial");
        assert!(partial.contains("LIBERADO_VAPID_PUBLIC_KEY"), "{partial}");
    }

    fn field(lines: &str, key: &str) -> String {
        lines
            .lines()
            .find_map(|line| line.strip_prefix(key))
            .expect("field")
            .to_string()
    }
}
