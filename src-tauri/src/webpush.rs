//! Web Push (RFC 8030) from this Mac, without a server in between:
//! messages are encrypted for the phone (RFC 8291, aes128gcm per RFC 8188)
//! and signed with this Mac's VAPID key (RFC 8292). Pure Rust crypto, so the
//! signed app links no external libraries.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes128Gcm, Nonce};
use base64::Engine;
use hkdf::Hkdf;
use p256::ecdsa::signature::Signer;
use p256::{PublicKey, SecretKey};
use sha2::Sha256;

const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;
/// One record holds the whole message (payloads are a few hundred bytes).
const RS: u32 = 4096;

/// Uncompressed public point (65 bytes) of a P-256 secret key.
fn public_bytes(sk: &SecretKey) -> Vec<u8> {
    use p256::elliptic_curve::sec1::ToSec1Point;
    sk.public_key().to_sec1_point(false).as_bytes().to_vec()
}

/// The VAPID private key (raw 32-byte scalar, base64url) as a secret key.
pub fn vapid_key(b64: &str) -> Option<SecretKey> {
    let raw = B64.decode(b64).ok()?;
    if raw.len() != 32 {
        return None;
    }
    SecretKey::from_slice(&raw).ok()
}

/// A new VAPID private key, base64url.
pub fn new_vapid_key() -> String {
    B64.encode(random_secret().to_bytes())
}

/// A random P-256 secret key from the OS-seeded generator. A random 32-byte
/// string is a valid scalar except with negligible probability; retry then.
fn random_secret() -> SecretKey {
    use rand::RngExt;
    let mut rng = rand::rng();
    loop {
        let mut b = [0u8; 32];
        rng.fill(&mut b);
        if let Ok(k) = SecretKey::from_slice(&b) {
            return k;
        }
    }
}

/// The application server key the browser subscribes with (base64url).
pub fn vapid_public(sk: &SecretKey) -> String {
    B64.encode(public_bytes(sk))
}

fn hkdf_expand(salt: &[u8], ikm: &[u8], info: &[u8], out: &mut [u8]) -> Result<(), String> {
    Hkdf::<Sha256>::new(Some(salt), ikm)
        .expand(info, out)
        .map_err(|_| "hkdf".to_string())
}

/// Encrypts `plaintext` for a subscription (`ua_public` = p256dh, `auth`)
/// with the given ephemeral key and salt. Returns the aes128gcm body.
fn encrypt_with(plaintext: &[u8], ua_public: &[u8], auth: &[u8], as_secret: &SecretKey, salt: &[u8; 16]) -> Result<Vec<u8>, String> {
    if plaintext.len() + 17 > RS as usize {
        return Err("message too long".into());
    }
    let ua = PublicKey::from_sec1_bytes(ua_public).map_err(|_| "bad p256dh key".to_string())?;
    let as_public = public_bytes(as_secret);
    let shared = p256::ecdh::diffie_hellman(as_secret.to_nonzero_scalar(), ua.as_affine());
    // RFC 8291 3.4: IKM from the ECDH secret, keyed by the auth secret.
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(&as_public);
    let mut ikm = [0u8; 32];
    hkdf_expand(auth, shared.raw_secret_bytes(), &key_info, &mut ikm)?;
    // RFC 8188 2.2 / 2.3: content key and nonce.
    let mut cek = [0u8; 16];
    let mut nonce = [0u8; 12];
    hkdf_expand(salt, &ikm, b"Content-Encoding: aes128gcm\0", &mut cek)?;
    hkdf_expand(salt, &ikm, b"Content-Encoding: nonce\0", &mut nonce)?;
    // Single (last) record: plaintext, then the 0x02 delimiter, no padding.
    let mut record = plaintext.to_vec();
    record.push(2);
    let cipher = Aes128Gcm::new_from_slice(&cek).map_err(|_| "aes key".to_string())?;
    let sealed = cipher
        .encrypt(&Nonce::from(nonce), record.as_slice())
        .map_err(|_| "encryption failed".to_string())?;
    let mut body = Vec::with_capacity(21 + 65 + sealed.len());
    body.extend_from_slice(salt);
    body.extend_from_slice(&RS.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(&as_public);
    body.extend_from_slice(&sealed);
    Ok(body)
}

/// Encrypts with a fresh ephemeral key and salt.
pub fn encrypt(plaintext: &[u8], p256dh_b64: &str, auth_b64: &str) -> Result<Vec<u8>, String> {
    let ua = B64.decode(p256dh_b64.trim_end_matches('=')).map_err(|_| "bad p256dh".to_string())?;
    let auth = B64.decode(auth_b64.trim_end_matches('=')).map_err(|_| "bad auth".to_string())?;
    if ua.len() != 65 || auth.len() != 16 {
        return Err("bad subscription keys".into());
    }
    let eph = random_secret();
    let mut salt = [0u8; 16];
    {
        use rand::RngExt;
        rand::rng().fill(&mut salt);
    }
    encrypt_with(plaintext, &ua, &auth, &eph, &salt)
}

/// `Authorization` header value for a push (RFC 8292): a JWT for the push
/// service's origin `aud` (e.g. `https://web.push.apple.com`), valid 12
/// hours, plus our public key.
pub fn vapid_header(sk: &SecretKey, aud: &str, subject: &str, now: u64) -> Result<String, String> {
    let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = serde_json::json!({ "aud": aud, "exp": now + 12 * 3600, "sub": subject });
    let claims = B64.encode(serde_json::to_vec(&claims).map_err(|e| e.to_string())?);
    let signing_input = format!("{header}.{claims}");
    let key = p256::ecdsa::SigningKey::from(sk.clone());
    let sig: p256::ecdsa::Signature = key.sign(signing_input.as_bytes());
    Ok(format!("vapid t={signing_input}.{}, k={}", B64.encode(sig.to_bytes()), vapid_public(sk)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Vec<u8> {
        B64.decode(s).unwrap()
    }

    /// The worked example in RFC 8291, Appendix A.
    #[test]
    fn matches_rfc8291_example() {
        let plaintext = b"When I grow up, I want to be a watermelon";
        let ua_public = d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let auth = d("BTBZMqHH6r4Tts7J_aSIgg");
        let as_secret = SecretKey::from_slice(&d("yfWPiYE-n46HLnH0KqZOF1fJJU3MYrct3AELtAQ-oRw")).unwrap();
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        assert_eq!(
            B64.encode(public_bytes(&as_secret)),
            "BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8"
        );
        let body = encrypt_with(plaintext, &ua_public, &auth, &as_secret, &salt).unwrap();
        assert_eq!(
            B64.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn vapid_jwt_verifies() {
        use p256::ecdsa::signature::Verifier;
        let sk = vapid_key(&new_vapid_key()).unwrap();
        let h = vapid_header(&sk, "https://web.push.apple.com", "https://example.org", 1000).unwrap();
        let t = h.strip_prefix("vapid t=").unwrap().split(", k=").next().unwrap();
        let (input, sig) = t.rsplit_once('.').unwrap();
        let claims: serde_json::Value = serde_json::from_slice(&d(input.split('.').nth(1).unwrap())).unwrap();
        assert_eq!(claims["aud"], "https://web.push.apple.com");
        assert_eq!(claims["exp"], 1000 + 12 * 3600);
        let sig = p256::ecdsa::Signature::from_slice(&d(sig)).unwrap();
        let vk = p256::ecdsa::VerifyingKey::from(&p256::ecdsa::SigningKey::from(sk.clone()));
        vk.verify(input.as_bytes(), &sig).unwrap();
    }

    #[test]
    fn rejects_bad_keys() {
        assert!(vapid_key("").is_none());
        assert!(vapid_key("AAAA").is_none());
        assert!(encrypt(b"x", "AAAA", "BTBZMqHH6r4Tts7J_aSIgg").is_err());
    }
}
