//! RFC 8291 `aes128gcm` body. One record. No extra padding.

use aws_lc_rs::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use aws_lc_rs::agreement::{self, UnparsedPublicKey};
use aws_lc_rs::hkdf::{self, KeyType};

const RECORD_SIZE: u32 = 4096;
const MAX_PLAINTEXT: usize = 3993;
const PUBLIC_LEN: usize = 65;

struct Len(usize);

impl KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

pub(crate) struct SealParts<'a> {
    pub plaintext: &'a [u8],
    pub ua_public: &'a [u8],
    pub auth: &'a [u8],
    pub as_private: &'a [u8],
    pub salt: &'a [u8],
}

pub(crate) struct Sealed {
    /// RFC 8291 intermediate. The vector test reads it. Production only needs `body`.
    #[cfg(test)]
    pub ecdh: Vec<u8>,
    /// RFC 8291 intermediate. The vector test reads it. Production only needs `body`.
    #[cfg(test)]
    pub cek: [u8; 16],
    /// RFC 8291 intermediate. The vector test reads it. Production only needs `body`.
    #[cfg(test)]
    pub nonce: [u8; 12],
    pub body: Vec<u8>,
}

pub(crate) fn seal(parts: SealParts<'_>) -> Result<Sealed, String> {
    require_lengths(&parts)?;
    let as_public = public_bytes(parts.as_private)?;
    let ecdh = shared_secret(parts.as_private, parts.ua_public)?;
    let derived = derive_keys(parts.auth, parts.salt, parts.ua_public, &as_public, &ecdh)?;
    let ciphertext = encrypt(&derived.cek, &derived.nonce, parts.plaintext)?;
    let mut body = header(parts.salt, &as_public);
    body.extend_from_slice(&ciphertext);
    Ok(Sealed {
        #[cfg(test)]
        ecdh,
        #[cfg(test)]
        cek: derived.cek,
        #[cfg(test)]
        nonce: derived.nonce,
        body,
    })
}

fn require_lengths(parts: &SealParts<'_>) -> Result<(), String> {
    require_plaintext(parts.plaintext)?;
    require_peer(parts.ua_public)?;
    require_secrets(parts)?;
    Ok(())
}

fn require_plaintext(plaintext: &[u8]) -> Result<(), String> {
    if plaintext.len() > MAX_PLAINTEXT {
        return Err("web push payload is too long".into());
    }
    Ok(())
}

fn require_peer(key: &[u8]) -> Result<(), String> {
    if key.len() == PUBLIC_LEN && key.first() == Some(&0x04) {
        return Ok(());
    }
    Err("subscription key is not a 65-byte P-256 key".into())
}

fn require_secrets(parts: &SealParts<'_>) -> Result<(), String> {
    if parts.auth.is_empty() {
        return Err("web push auth secret is empty".into());
    }
    if parts.salt.len() != 16 || parts.as_private.len() != 32 {
        return Err("web push key material has the wrong length".into());
    }
    Ok(())
}

fn public_bytes(scalar: &[u8]) -> Result<Vec<u8>, String> {
    let private = agreement::PrivateKey::from_private_key(&agreement::ECDH_P256, scalar)
        .map_err(|_| "application server key rejected".to_string())?;
    let public = private
        .compute_public_key()
        .map_err(|_| "application server public key failed".to_string())?;
    Ok(public.as_ref().to_vec())
}

fn shared_secret(scalar: &[u8], peer: &[u8]) -> Result<Vec<u8>, String> {
    let private = agreement::PrivateKey::from_private_key(&agreement::ECDH_P256, scalar)
        .map_err(|_| "application server key rejected".to_string())?;
    let peer = UnparsedPublicKey::new(&agreement::ECDH_P256, peer);
    agreement::agree(&private, peer, "ecdh failed".to_string(), |secret| {
        Ok(secret.to_vec())
    })
}

struct Derived {
    cek: [u8; 16],
    nonce: [u8; 12],
}

fn derive_keys(
    auth: &[u8],
    salt: &[u8],
    ua_public: &[u8],
    as_public: &[u8],
    ecdh: &[u8],
) -> Result<Derived, String> {
    let mut ikm = [0u8; 32];
    expand(auth, ecdh, &webpush_info(ua_public, as_public), &mut ikm)?;
    let mut cek = [0u8; 16];
    expand(salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek)?;
    let mut nonce = [0u8; 12];
    expand(salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce)?;
    Ok(Derived { cek, nonce })
}

fn webpush_info(ua_public: &[u8], as_public: &[u8]) -> Vec<u8> {
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(ua_public);
    info.extend_from_slice(as_public);
    info
}

fn expand(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) -> Result<(), String> {
    if salt.is_empty() {
        return Err("hkdf salt is empty".into());
    }
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let info_parts: &[&[u8]] = &[info];
    let okm = prk
        .expand(info_parts, Len(out.len()))
        .map_err(|_| "hkdf expand failed".to_string())?;
    okm.fill(out).map_err(|_| "hkdf fill failed".to_string())
}

fn encrypt(cek: &[u8], nonce: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let key = UnboundKey::new(&AES_128_GCM, cek).map_err(|_| "content key rejected".to_string())?;
    let key = LessSafeKey::new(key);
    let nonce =
        Nonce::try_assume_unique_for_key(nonce).map_err(|_| "nonce rejected".to_string())?;
    let mut body = plaintext.to_vec();
    body.push(0x02);
    key.seal_in_place_append_tag(nonce, Aad::empty(), &mut body)
        .map_err(|_| "aes128gcm seal failed".to_string())?;
    Ok(body)
}

fn header(salt: &[u8], as_public: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(86);
    out.extend_from_slice(salt);
    out.extend_from_slice(&RECORD_SIZE.to_be_bytes());
    out.push(u8::try_from(as_public.len()).unwrap_or(0));
    out.extend_from_slice(as_public);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::web_push::b64;

    fn stripped(text: &str) -> String {
        text.split_whitespace().collect()
    }

    #[test]
    fn rfc8291_vector_matches_the_body_and_the_intermediates() {
        let as_private = b64::decode("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw").expect("as");
        let ua_public = b64::decode(&stripped(
            "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcx aOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4",
        ))
        .expect("ua");
        let auth = b64::decode("BTBZMqHH6r4Tts7J_aSIgg").expect("auth");
        let salt = b64::decode("DGv6ra1nlYgDCS1FRnbzlw").expect("salt");
        let sealed = seal(SealParts {
            plaintext: b"When I grow up, I want to be a watermelon",
            ua_public: &ua_public,
            auth: &auth,
            as_private: &as_private,
            salt: &salt,
        })
        .expect("seal");
        assert_eq!(
            b64::encode(&sealed.ecdh),
            "kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs"
        );
        assert_eq!(b64::encode(&sealed.cek), "oIhVW04MRdy2XN9CiKLxTg");
        assert_eq!(b64::encode(&sealed.nonce), "4h_95klXJ5E_qnoN");
        assert!(sealed.body.len() > 86);
        let expected = stripped(
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN",
        );
        assert_eq!(b64::encode(&sealed.body), expected);
    }
}
