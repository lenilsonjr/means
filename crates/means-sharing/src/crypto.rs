//! Strict RFC 7515 compact JWS adapter and library-provided age encryption.
use age::secrecy::ExposeSecret;
use anyhow::{bail, ensure, Context, Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD as B64, Engine};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use zeroize::{Zeroize, Zeroizing};

pub const MAX_BUNDLE: usize = 128 * 1024 * 1024;
pub const MAX_IDENTITY: usize = 64 * 1024;
pub const DELIVERY: &str = "means-vault-v1+jws";
pub const RECEIPT: &str = "means-receipt-v1+jws";
pub const ROTATION: &str = "means-rotation-v1+jws";
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Public {
    pub version: String,
    pub signing_algorithm: String,
    pub signing_key: String,
    pub encryption_algorithm: String,
    pub recipient: String,
}
impl Public {
    pub fn fingerprint(&self) -> Result<String> {
        self.validate()?;
        Ok(digest(&canonical(self)?))
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.version == "1" && self.signing_algorithm == "Ed25519" && self.encryption_algorithm == "X25519-age", "unsupported public identity");
        let _: age::x25519::Recipient = self.recipient.parse().map_err(|_| anyhow::anyhow!("invalid age recipient"))?;
        let bytes: [u8; 32] = B64.decode(&self.signing_key)?.try_into().map_err(|_| anyhow::anyhow!("invalid signing key"))?;
        let key = VerifyingKey::from_bytes(&bytes)?;
        ensure!(!key.is_weak(), "weak signing key");
        ensure!(B64.encode(bytes) == self.signing_key, "noncanonical public key");
        Ok(())
    }
}
#[derive(Serialize, Deserialize, Zeroize)]
#[serde(deny_unknown_fields)]
struct Private {
    version: String,
    signing_secret: String,
    encryption_secret: String,
    #[zeroize(skip)]
    public: Public,
}
pub struct Identity {
    signing: SigningKey,
    encryption: age::x25519::Identity,
    pub public: Public,
}
impl Identity {
    pub fn generate() -> Self {
        let signing = SigningKey::generate(&mut rand_core::OsRng);
        let encryption = age::x25519::Identity::generate();
        let public = Public {
            version: "1".into(),
            signing_algorithm: "Ed25519".into(),
            signing_key: B64.encode(signing.verifying_key().as_bytes()),
            encryption_algorithm: "X25519-age".into(),
            recipient: encryption.to_public().to_string(),
        };
        Self { signing, encryption, public }
    }
    pub fn protect(&self, passphrase: &str) -> Result<Vec<u8>> {
        ensure!(passphrase.chars().count() >= 12, "use a passphrase of at least 12 characters");
        let value = Zeroizing::new(Private {
            version: "1".into(),
            signing_secret: B64.encode(self.signing.to_bytes()),
            encryption_secret: self.encryption.to_string().expose_secret().to_string(),
            public: self.public.clone(),
        });
        let plain = Zeroizing::new(canonical(&*value)?);
        let mut recipient = age::scrypt::Recipient::new(passphrase.to_owned().into());
        recipient.set_work_factor(if cfg!(test) { 10 } else { 18 });
        encrypt(&recipient, &plain)
    }
    pub fn unlock(data: &[u8], passphrase: &str) -> Result<Self> {
        ensure!(data.len() <= MAX_IDENTITY, "identity file too large");
        let mut key = age::scrypt::Identity::new(passphrase.to_owned().into());
        key.set_max_work_factor(18);
        let plain = Zeroizing::new(decrypt(&key, data, MAX_IDENTITY).context("cannot unlock identity")?);
        let p = Zeroizing::new(parse::<Private>(&plain)?);
        ensure!(p.version == "1", "unsupported private identity");
        let seed = Zeroizing::new(B64.decode(&p.signing_secret)?);
        let seed: &[u8; 32] = seed.as_slice().try_into().map_err(|_| anyhow::anyhow!("invalid signing seed"))?;
        let signing = SigningKey::from_bytes(seed);
        let encryption: age::x25519::Identity = p.encryption_secret.parse().map_err(|_| anyhow::anyhow!("invalid age identity"))?;
        p.public.validate()?;
        ensure!(B64.encode(signing.verifying_key().as_bytes()) == p.public.signing_key && encryption.to_public().to_string() == p.public.recipient, "private/public identity mismatch");
        Ok(Self { signing, encryption, public: p.public.clone() })
    }
    pub fn sign<T: Serialize>(&self, typ: &str, value: &T) -> Result<Vec<u8>> {
        let header = canonical(&serde_json::json!({"alg":"Ed25519","typ":typ}))?;
        let payload = canonical(value)?;
        let input = format!("{}.{}", B64.encode(header), B64.encode(payload));
        Ok(format!("{}.{}", input, B64.encode(self.signing.sign(input.as_bytes()).to_bytes())).into_bytes())
    }
    pub fn decrypt(&self, data: &[u8]) -> Result<Vec<u8>> {
        decrypt(&self.encryption, data, MAX_BUNDLE)
    }
}
pub fn canonical<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    Ok(serde_jcs::to_vec(value)?)
}
pub fn parse<T: DeserializeOwned + Serialize>(bytes: &[u8]) -> Result<T> {
    let value: T = serde_json::from_slice(bytes)?;
    ensure!(canonical(&value)? == bytes, "noncanonical JSON or duplicate/unknown fields");
    Ok(value)
}
pub fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn verify<T: DeserializeOwned + Serialize>(public: &Public, typ: &str, compact: &[u8]) -> Result<T> {
    public.validate()?;
    ensure!(compact.len() <= MAX_BUNDLE, "JWS too large");
    let text = std::str::from_utf8(compact)?;
    let parts: Vec<_> = text.splitn(4, '.').collect();
    ensure!(parts.len() == 3, "invalid compact JWS");
    let expected = B64.encode(canonical(&serde_json::json!({"alg":"Ed25519","typ":typ}))?);
    ensure!(parts[0] == expected, "unsupported JWS protected header");
    let key: [u8; 32] = B64.decode(&public.signing_key)?.try_into().map_err(|_| anyhow::anyhow!("invalid signing key"))?;
    let sig = B64.decode(parts[2])?;
    ensure!(B64.encode(&sig) == parts[2], "noncanonical signature encoding");
    let input = &text[..parts[0].len() + 1 + parts[1].len()];
    VerifyingKey::from_bytes(&key)?.verify_strict(input.as_bytes(), &Signature::from_slice(&sig)?).context("invalid owner signature")?;
    let payload = B64.decode(parts[1])?;
    ensure!(B64.encode(&payload) == parts[1], "noncanonical payload encoding");
    parse(&payload)
}
pub fn encrypt(recipient: &dyn age::Recipient, plain: &[u8]) -> Result<Vec<u8>> {
    ensure!(plain.len() <= MAX_BUNDLE, "plaintext too large");
    let enc = age::Encryptor::with_recipients(std::iter::once(recipient))?;
    let mut out = Vec::new();
    let mut writer = enc.wrap_output(&mut out)?;
    writer.write_all(plain)?;
    writer.finish()?;
    ensure!(out.len() <= MAX_BUNDLE, "encrypted file too large");
    Ok(out)
}
pub fn encrypt_to(public: &Public, plain: &[u8]) -> Result<Vec<u8>> {
    public.validate()?;
    let recipient: age::x25519::Recipient = public.recipient.parse().map_err(|_| anyhow::anyhow!("invalid recipient"))?;
    encrypt(&recipient, plain)
}
fn decrypt(identity: &dyn age::Identity, data: &[u8], limit: usize) -> Result<Vec<u8>> {
    ensure!(data.len() <= limit, "encrypted input too large");
    let decryptor = age::Decryptor::new(data)?;
    let mut reader = decryptor.decrypt(std::iter::once(identity))?;
    let mut plain = Zeroizing::new(Vec::new());
    reader.by_ref().take(limit as u64 + 1).read_to_end(&mut plain)?;
    if plain.len() > limit {
        plain.zeroize();
        bail!("decrypted input too large")
    }
    // Reading to EOF verifies the final authenticated chunk, including empty final chunks.
    Ok(std::mem::take(&mut *plain))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn independent_joserfc_vector_and_header_pinning() {
        let vector: serde_json::Value = serde_json::from_str(include_str!("../test-vectors/ed25519-jws.json")).unwrap();
        let mut id = Identity::generate();
        let seed: [u8; 32] = hex::decode(vector["seed"].as_str().unwrap()).unwrap().try_into().unwrap();
        id.signing = SigningKey::from_bytes(&seed);
        id.public.signing_key = vector["public"].as_str().unwrap().into();
        let compact = vector["compact"].as_str().unwrap().as_bytes();
        assert_eq!(id.sign(DELIVERY, &vector["payload"]).unwrap(), compact);
        assert_eq!(verify::<serde_json::Value>(&id.public, DELIVERY, compact).unwrap(), vector["payload"]);
        assert!(verify::<serde_json::Value>(&Identity::generate().public, DELIVERY, compact).is_err());
        assert!(verify::<serde_json::Value>(&id.public, RECEIPT, compact).is_err());
        for header in
            [serde_json::json!({"alg":"EdDSA","typ":DELIVERY}), serde_json::json!({"alg":"none","typ":DELIVERY}), serde_json::json!({"alg":"Ed25519","typ":DELIVERY,"jku":"https://untrusted.invalid"})]
        {
            let text = std::str::from_utf8(compact).unwrap();
            let suffix = text.split_once('.').unwrap().1;
            let altered = format!("{}.{}", B64.encode(canonical(&header).unwrap()), suffix);
            assert!(verify::<serde_json::Value>(&id.public, DELIVERY, altered.as_bytes()).is_err());
        }
        let mut changed = compact.to_vec();
        let index = changed.len() / 2;
        changed[index] = if changed[index] == b'A' { b'B' } else { b'A' };
        assert!(verify::<serde_json::Value>(&id.public, DELIVERY, &changed).is_err());
    }
    #[test]
    fn private_backup_and_full_age_authentication() {
        let id = Identity::generate();
        let encrypted = id.protect("long test passphrase").unwrap();
        assert!(Identity::unlock(&encrypted, "wrong passphrase").is_err());
        let recovered = Identity::unlock(&encrypted, "long test passphrase").unwrap();
        assert_eq!(recovered.public, id.public);
        let data = vec![42; 70_000];
        let bundle = encrypt_to(&id.public, &data).unwrap();
        assert_eq!(recovered.decrypt(&bundle).unwrap(), data);
        for end in [0, 1, bundle.len() / 2, bundle.len() - 1] {
            assert!(recovered.decrypt(&bundle[..end]).is_err());
        }
        let mut changed = bundle.clone();
        *changed.last_mut().unwrap() ^= 1;
        assert!(recovered.decrypt(&changed).is_err());
        assert!(Identity::generate().decrypt(&bundle).is_err());
        assert!(id.protect("short").is_err());
    }
    #[test]
    fn strict_canonical_data_rejects_duplicates_unknowns_and_numbers() {
        let p = Identity::generate().public;
        let bytes = canonical(&p).unwrap();
        assert_eq!(parse::<Public>(&bytes).unwrap(), p);
        let mut text = String::from_utf8(bytes).unwrap();
        text.insert_str(1, "\"version\":\"1\",");
        assert!(parse::<Public>(text.as_bytes()).is_err());
        let mut value = serde_json::to_value(&p).unwrap();
        value["extra"] = true.into();
        assert!(parse::<Public>(&canonical(&value).unwrap()).is_err());
        value.as_object_mut().unwrap().remove("extra");
        value["version"] = 1.into();
        assert!(parse::<Public>(&serde_json::to_vec(&value).unwrap()).is_err());
    }
}
