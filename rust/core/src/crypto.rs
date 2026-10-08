//! Cryptographic primitives the Moonlight protocol needs: hashing, the
//! pairing PIN key, AES-ECB/GCM/CBC, the host's RSA identity and X.509
//! certificate helpers.
use aes::{
    Aes128,
    cipher::{Block, BlockDecrypt, BlockEncrypt, KeyInit},
};
use aes_gcm::{
    Aes128Gcm, AesGcm, Nonce,
    aead::{AeadInPlace, consts::U16},
};
use anyhow::{Context, Result, bail};
use rand::{RngCore, rngs::OsRng};
use rsa::{
    RsaPrivateKey, RsaPublicKey,
    pkcs1::DecodeRsaPrivateKey,
    pkcs1v15::{Signature, SigningKey, VerifyingKey},
    pkcs8::{DecodePrivateKey, DecodePublicKey, EncodePrivateKey},
    signature::{RandomizedSigner, SignatureEncoding, Verifier},
};
use sha2::{Digest, Sha256};
use std::path::Path;
use subtle::ConstantTimeEq;

pub fn random<const N: usize>() -> [u8; N] {
    let mut b = [0; N];
    OsRng.fill_bytes(&mut b);
    b
}
pub fn hash(b: &[u8]) -> [u8; 32] {
    Sha256::digest(b).into()
}
/// Sunshine's util::hex(array) serializes the digest in reverse byte order.
pub fn legacy_hash(input: &[u8]) -> String {
    let mut digest = hash(input);
    digest.reverse();
    hex::encode_upper(digest)
}
/// Accept previous C++ state and credentials written by early Rust candidates.
pub fn matches_hash(input: &[u8], stored: &str) -> bool {
    let mut digest = hash(input);
    let forward = hex::encode(digest);
    digest.reverse();
    let legacy = hex::encode(digest);
    let stored = stored.to_ascii_lowercase();
    equal(stored.as_bytes(), forward.as_bytes()) | equal(stored.as_bytes(), legacy.as_bytes())
}
pub fn equal(a: &[u8], b: &[u8]) -> bool {
    bool::from(a.ct_eq(b))
}
pub fn pin_key(salt: &[u8; 16], pin: &str) -> [u8; 16] {
    let mut b = salt.to_vec();
    b.extend_from_slice(pin.as_bytes());
    hash(&b)[..16].try_into().unwrap()
}
pub fn ecb(key: &[u8; 16], data: &[u8], encrypt: bool) -> Result<Vec<u8>> {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        bail!("AES-ECB data is not block aligned");
    }
    let c = Aes128::new_from_slice(key).unwrap();
    let mut b = data.to_vec();
    for chunk in b.as_chunks_mut::<16>().0 {
        let block = Block::<Aes128>::from_mut_slice(chunk);
        if encrypt {
            c.encrypt_block(block)
        } else {
            c.decrypt_block(block)
        }
    }
    Ok(b)
}
pub fn gcm_seal(key: &[u8; 16], nonce: &[u8], data: &[u8]) -> Result<([u8; 16], Vec<u8>)> {
    let mut b = data.to_vec();
    let tag = match nonce.len() {
        12 => Aes128Gcm::new_from_slice(key)
            .unwrap()
            .encrypt_in_place_detached(Nonce::from_slice(nonce), &[], &mut b),
        16 => AesGcm::<Aes128, U16>::new_from_slice(key)
            .unwrap()
            .encrypt_in_place_detached(Nonce::<U16>::from_slice(nonce), &[], &mut b),
        _ => bail!("unsupported AES-GCM nonce size"),
    }
    .map_err(|_| anyhow::anyhow!("AES-GCM encryption failed"))?;
    Ok((tag.into(), b))
}
/// Video packets share a key but always have independent 96-bit nonces.
pub(crate) struct PacketSealer(Aes128Gcm);
impl PacketSealer {
    #[inline]
    pub(crate) fn new(key: &[u8; 16]) -> Self {
        Self(Aes128Gcm::new_from_slice(key).unwrap())
    }
    #[inline]
    pub(crate) fn seal(&self, nonce: &[u8; 12], bytes: &mut [u8]) -> Result<[u8; 16]> {
        self.0
            .encrypt_in_place_detached(Nonce::from_slice(nonce), &[], bytes)
            .map(Into::into)
            .map_err(|_| anyhow::anyhow!("AES-GCM encryption failed"))
    }
}
pub fn gcm_open(key: &[u8; 16], nonce: &[u8], tag: &[u8], data: &[u8]) -> Result<Vec<u8>> {
    if tag.len() != 16 {
        bail!("invalid authentication tag");
    }
    let mut b = data.to_vec();
    let tag = aes_gcm::Tag::from_slice(tag);
    match nonce.len() {
        12 => Aes128Gcm::new_from_slice(key)
            .unwrap()
            .decrypt_in_place_detached(Nonce::from_slice(nonce), &[], &mut b, tag),
        16 => AesGcm::<Aes128, U16>::new_from_slice(key)
            .unwrap()
            .decrypt_in_place_detached(Nonce::<U16>::from_slice(nonce), &[], &mut b, tag),
        _ => bail!("unsupported AES-GCM nonce size"),
    }
    .map_err(|_| anyhow::anyhow!("authentication failed"))?;
    Ok(b)
}
pub fn cbc_seal(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Vec<u8> {
    use aes::cipher::{BlockEncryptMut, KeyIvInit, block_padding::Pkcs7};
    cbc::Encryptor::<Aes128>::new(key.into(), iv.into()).encrypt_padded_vec_mut::<Pkcs7>(data)
}
pub fn cbc_open(key: &[u8; 16], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
    cbc::Decryptor::<Aes128>::new(key.into(), iv.into())
        .decrypt_padded_vec_mut::<Pkcs7>(data)
        .map_err(|_| anyhow::anyhow!("invalid CBC payload"))
}
#[derive(Clone)]
pub struct Identity {
    pub certificate: String,
    pub private_pem: String,
    pub der: Vec<u8>,
    pub signature: Vec<u8>,
    private: RsaPrivateKey,
}
impl Identity {
    pub fn load(cert: &Path, key: &Path) -> Result<Self> {
        if let Some(id) = Self::read(cert, key)? {
            return Ok(id);
        }
        let id = Self::generate()?;
        crate::state::atomic_write(key, id.private_pem.as_bytes())?;
        crate::state::atomic_write(cert, id.certificate.as_bytes())?;
        Ok(id)
    }
    /// The identity in `cert` and `key`; None when neither exists yet.
    pub fn read(cert: &Path, key: &Path) -> Result<Option<Self>> {
        match (std::fs::read_to_string(cert), std::fs::read_to_string(key)) {
            (Ok(c), Ok(k)) => Self::from_pem(c, k).map(Some),
            (Err(c), Err(k))
                if c.kind() == std::io::ErrorKind::NotFound
                    && k.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(None)
            }
            _ => bail!(
                "certificate and key must both exist and be readable; refusing to replace identity"
            ),
        }
    }
    pub fn generate() -> Result<Self> {
        let private = RsaPrivateKey::new(&mut OsRng, 2048)?;
        let private_pem = private
            .to_pkcs8_pem(rsa::pkcs8::LineEnding::LF)?
            .to_string();
        let pair = rcgen::KeyPair::from_pem(&private_pem)?;
        let mut params =
            rcgen::CertificateParams::new(vec!["Sunshine".to_owned(), "localhost".to_owned()])?;
        params
            .distinguished_name
            .push(rcgen::DnType::CommonName, "Sunshine");
        let cert = params.self_signed(&pair)?;
        Self::from_pem(cert.pem(), private_pem)
    }
    pub fn from_pem(certificate: String, private_pem: String) -> Result<Self> {
        let private = RsaPrivateKey::from_pkcs8_pem(&private_pem)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(&private_pem))?;
        let der = certificate_der(&certificate)?;
        let (_, cert) = x509_parser::parse_x509_certificate(&der)
            .map_err(|_| anyhow::anyhow!("invalid certificate"))?;
        let signature = cert.signature_value.data.to_vec();
        let public = public_key(&certificate)?;
        if public != RsaPublicKey::from(&private) {
            bail!("certificate does not match private key");
        }
        Ok(Self {
            certificate,
            private_pem,
            der,
            signature,
            private,
        })
    }
    pub fn sign(&self, b: &[u8]) -> Vec<u8> {
        SigningKey::<Sha256>::new(self.private.clone())
            .sign_with_rng(&mut OsRng, b)
            .to_vec()
    }
}
pub fn certificate_der(pem_text: &str) -> Result<Vec<u8>> {
    let b = pem::parse(pem_text)?;
    if b.tag() != "CERTIFICATE" {
        bail!("expected certificate PEM");
    }
    Ok(b.into_contents())
}
pub fn public_key(pem_text: &str) -> Result<RsaPublicKey> {
    let der = certificate_der(pem_text)?;
    let (_, cert) = x509_parser::parse_x509_certificate(&der)
        .map_err(|_| anyhow::anyhow!("invalid X.509 certificate"))?;
    RsaPublicKey::from_public_key_der(cert.tbs_certificate.subject_pki.raw)
        .context("pairing certificate must contain an RSA key")
}
pub fn certificate_signature(pem_text: &str) -> Result<Vec<u8>> {
    let der = certificate_der(pem_text)?;
    let (_, c) = x509_parser::parse_x509_certificate(&der)
        .map_err(|_| anyhow::anyhow!("invalid X.509 certificate"))?;
    Ok(c.signature_value.data.to_vec())
}
pub fn verify(pem_text: &str, data: &[u8], signature: &[u8]) -> Result<bool> {
    Ok(VerifyingKey::<Sha256>::new(public_key(pem_text)?)
        .verify(data, &Signature::try_from(signature)?)
        .is_ok())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn nist_aes_gcm_vector() {
        let (tag, b) = gcm_seal(&[0; 16], &[0; 12], &[0; 16]).unwrap();
        assert_eq!(hex::encode(b), "0388dace60b6a392f328c2b971b2fe78");
        assert_eq!(hex::encode(tag), "ab6e47d42cec13bdf53a67b21257bddf");
    }
    #[test]
    fn reject_forged_tag_before_parsing() {
        let (mut t, b) = gcm_seal(&[3; 16], &[4; 12], b"authenticated input").unwrap();
        t[0] ^= 1;
        assert!(gcm_open(&[3; 16], &[4; 12], &t, &b).is_err());
    }
    #[test]
    fn legacy_nonce_and_audio_round_trip() {
        let k = [4; 16];
        let (t, b) = gcm_seal(&k, &[6; 16], b"old client").unwrap();
        assert_eq!(gcm_open(&k, &[6; 16], &t, &b).unwrap(), b"old client");
        let b = cbc_seal(&k, &[1; 16], b"opus");
        assert_eq!(cbc_open(&k, &[1; 16], &b).unwrap(), b"opus");
    }
}
