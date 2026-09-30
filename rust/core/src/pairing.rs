use crate::crypto;
use anyhow::{Result, bail};
use std::{
    collections::HashMap,
    time::{Duration, Instant},
};
use zeroize::Zeroize;

#[derive(Debug, Copy, Clone, PartialEq)]
enum Phase {
    Cert,
    Challenge,
    Response,
    Complete,
}
pub struct Pairing {
    pub unique_id: String,
    pub name: String,
    pub certificate: String,
    pub created: Instant,
    phase: Phase,
    key: [u8; 16],
    server_secret: [u8; 16],
    server_challenge: [u8; 16],
    client_hash: Vec<u8>,
}
impl Drop for Pairing {
    fn drop(&mut self) {
        self.key.zeroize();
        self.server_secret.zeroize();
    }
}
impl Pairing {
    pub fn new(
        unique_id: String,
        name: String,
        certificate: String,
        salt: &[u8],
        pin: &str,
    ) -> Result<Self> {
        if unique_id.is_empty()
            || unique_id.len() > 256
            || !unique_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            bail!("invalid client identity");
        }
        if pin.len() != 4 || !pin.bytes().all(|c| c.is_ascii_digit()) {
            bail!("PIN must contain four digits");
        }
        if salt.len() < 16 {
            bail!("salt is too short");
        }
        crypto::public_key(&certificate)?;
        Ok(Self {
            unique_id,
            name,
            certificate,
            created: Instant::now(),
            phase: Phase::Cert,
            key: crypto::pin_key(salt[..16].try_into().unwrap(), pin),
            server_secret: crypto::random(),
            server_challenge: crypto::random(),
            client_hash: Vec::new(),
        })
    }
    fn require(&mut self, phase: Phase) -> Result<()> {
        if self.phase != phase || self.created.elapsed() > Duration::from_secs(300) {
            self.phase = Phase::Complete;
            bail!("pairing request is expired or out of order");
        }
        Ok(())
    }
    pub fn client_challenge(&mut self, id: &crypto::Identity, challenge: &[u8]) -> Result<Vec<u8>> {
        self.require(Phase::Cert)?;
        if challenge.len() != 16 {
            self.phase = Phase::Complete;
            bail!("invalid challenge length");
        }
        let mut data = crypto::ecb(&self.key, challenge, false)?;
        data.extend_from_slice(&id.signature);
        data.extend_from_slice(&self.server_secret);
        let mut response = crypto::hash(&data).to_vec();
        response.extend_from_slice(&self.server_challenge);
        self.phase = Phase::Challenge;
        crypto::ecb(&self.key, &response, true)
    }
    pub fn server_response(&mut self, id: &crypto::Identity, response: &[u8]) -> Result<Vec<u8>> {
        self.require(Phase::Challenge)?;
        if response.len() != 32 {
            self.phase = Phase::Complete;
            bail!("invalid challenge response length");
        }
        self.client_hash = crypto::ecb(&self.key, response, false)?;
        let mut secret = self.server_secret.to_vec();
        secret.extend_from_slice(&id.sign(&self.server_secret));
        self.phase = Phase::Response;
        Ok(secret)
    }
    pub fn finish(&mut self, secret: &[u8]) -> Result<()> {
        self.require(Phase::Response)?;
        self.phase = Phase::Complete;
        if secret.len() != 16 + 256 {
            bail!("invalid RSA pairing secret length");
        }
        let mut data = self.server_challenge.to_vec();
        data.extend_from_slice(&crypto::certificate_signature(&self.certificate)?);
        data.extend_from_slice(&secret[..16]);
        if !crypto::equal(&crypto::hash(&data), &self.client_hash)
            || !crypto::verify(&self.certificate, &secret[..16], &secret[16..])?
        {
            bail!("client proof or PIN verification failed");
        }
        Ok(())
    }
}
#[derive(Default)]
pub struct Pairings {
    pub sessions: HashMap<String, Pairing>,
}
impl Pairings {
    pub fn expire(&mut self) {
        self.sessions
            .retain(|_, s| s.created.elapsed() < Duration::from_secs(300));
    }
    pub fn insert(&mut self, s: Pairing) -> Result<()> {
        self.expire();
        if self.sessions.contains_key(&s.unique_id) {
            bail!("pairing already in progress");
        }
        if self.sessions.len() >= 32 {
            bail!("too many pending pairing sessions");
        }
        self.sessions.insert(s.unique_id.clone(), s);
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pair_with_real_rsa_certificates_and_existing_wire_format() {
        let server = crypto::Identity::generate().unwrap();
        let client = crypto::Identity::generate().unwrap();
        let salt = [5; 16];
        let key = crypto::pin_key(&salt, "1234");
        let mut p = Pairing::new(
            "client-1".into(),
            "Moonlight".into(),
            client.certificate.clone(),
            &salt,
            "1234",
        )
        .unwrap();
        let challenge = [7; 16];
        let response = p
            .client_challenge(&server, &crypto::ecb(&key, &challenge, true).unwrap())
            .unwrap();
        let response = crypto::ecb(&key, &response, false).unwrap();
        let client_secret = [9; 16];
        let mut expected = challenge.to_vec();
        expected.extend_from_slice(&server.signature);
        expected.extend_from_slice(&p.server_secret);
        assert_eq!(&response[..32], &crypto::hash(&expected));
        let mut proof = response[32..].to_vec();
        proof.extend_from_slice(&client.signature);
        proof.extend_from_slice(&client_secret);
        let reply = p
            .server_response(
                &server,
                &crypto::ecb(&key, &crypto::hash(&proof), true).unwrap(),
            )
            .unwrap();
        assert!(crypto::verify(&server.certificate, &reply[..16], &reply[16..]).unwrap());
        let mut secret = client_secret.to_vec();
        secret.extend_from_slice(&client.sign(&client_secret));
        p.finish(&secret).unwrap();
        assert!(p.finish(&secret).is_err());
    }
}
