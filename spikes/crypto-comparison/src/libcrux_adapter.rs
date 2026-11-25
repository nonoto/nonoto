//! libcruxの系統 (`hpke-rs` と `hpke-rs-libcrux`・`libcrux-ed25519`・`libcrux-aead`・`libcrux-hkdf`) のアダプター。

use hpke_rs::hpke_types::{AeadAlgorithm as HpkeAead, KdfAlgorithm, KemAlgorithm};
use hpke_rs::{Hpke, HpkeKeyPair, HpkePrivateKey, HpkePublicKey, Mode};
use hpke_rs_libcrux::HpkeLibcrux;
use libcrux_aead::aesgcm256::AesGcm256;
use libcrux_aead::chacha20poly1305::ChaCha20Poly1305;
use libcrux_aead::xchacha20poly1305::XChaCha20Poly1305;
use libcrux_traits::aead::slice::Aead;
use zeroize::Zeroizing;

use crate::{
    AeadAlgorithm, CryptoError, CryptoFamily, HpkeKem, Sealed, SecretBytes, SigningKeyPair,
    WrapKeyPair, array, hkdf_sha256_output,
};

/// libcruxの系統。
#[derive(Debug, Clone, Copy)]
pub struct Libcrux;

fn hpke(kem: HpkeKem) -> Result<Hpke<HpkeLibcrux>, CryptoError> {
    let kem = match kem {
        HpkeKem::XWing => KemAlgorithm::XWingDraft06,
        HpkeKem::DhKemX25519 => KemAlgorithm::DhKem25519,
    };
    Hpke::try_new(
        Mode::Base,
        kem,
        KdfAlgorithm::HkdfSha256,
        HpkeAead::ChaCha20Poly1305,
    )
    .map_err(|_| CryptoError::Random)
}

fn wrap_keypair(keypair: HpkeKeyPair) -> WrapKeyPair {
    let (secret_key, public_key) = keypair.into_keys();
    WrapKeyPair {
        secret_key: SecretBytes::from_slice(secret_key.as_slice()),
        public_key: public_key.as_slice().to_vec(),
    }
}

fn encrypt<A: Aead>(
    key: &SecretBytes,
    nonce: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let mut out = vec![0; plaintext.len() + AeadAlgorithm::TAG_LEN];
    let (ciphertext, tag) = out.split_at_mut(plaintext.len());
    A::encrypt(ciphertext, tag, key.as_bytes(), nonce, aad, plaintext)
        .map_err(|_| CryptoError::Encrypt)?;
    Ok(out)
}

fn decrypt<A: Aead>(
    key: &SecretBytes,
    nonce: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<SecretBytes, CryptoError> {
    let Some(len) = ciphertext.len().checked_sub(AeadAlgorithm::TAG_LEN) else {
        return Err(CryptoError::Decrypt);
    };
    let (ciphertext, tag) = ciphertext.split_at(len);
    let mut plaintext = Zeroizing::new(vec![0; len]);
    A::decrypt(&mut plaintext, key.as_bytes(), nonce, aad, ciphertext, tag)
        .map_err(|_| CryptoError::Decrypt)?;
    Ok(SecretBytes::new(std::mem::take(&mut plaintext)))
}

impl CryptoFamily for Libcrux {
    const NAME: &'static str = "libcrux";
    const VERIFY_ENTRY: &'static str = "libcrux-ed25519の `verify` (入口は1つ)";

    fn generate_wrap_keypair(kem: HpkeKem) -> Result<WrapKeyPair, CryptoError> {
        hpke(kem)?
            .generate_key_pair()
            .map(wrap_keypair)
            .map_err(|_| CryptoError::Random)
    }

    fn derive_wrap_keypair(kem: HpkeKem, ikm: &[u8]) -> Result<WrapKeyPair, CryptoError> {
        hpke(kem)?
            .derive_key_pair(ikm)
            .map(wrap_keypair)
            .map_err(|_| CryptoError::InvalidInput("鍵の導出の入力"))
    }

    fn seal(
        kem: HpkeKem,
        public_key: &[u8],
        info: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Sealed, CryptoError> {
        let public_key = HpkePublicKey::new(public_key.to_vec());
        let (enc, ciphertext) = hpke(kem)?
            .seal(&public_key, info, aad, plaintext, None, None, None)
            .map_err(|_| CryptoError::Seal)?;
        Ok(Sealed { enc, ciphertext })
    }

    fn open(
        kem: HpkeKem,
        secret_key: &SecretBytes,
        enc: &[u8],
        info: &[u8],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<SecretBytes, CryptoError> {
        let secret_key = HpkePrivateKey::new(secret_key.as_bytes().to_vec());
        hpke(kem)?
            .open(enc, &secret_key, info, aad, ciphertext, None, None, None)
            .map(SecretBytes::new)
            .map_err(|_| CryptoError::Open)
    }

    fn signing_keypair_from_seed(seed: &SecretBytes) -> Result<SigningKeyPair, CryptoError> {
        let secret = Zeroizing::new(array::<32>(seed.as_bytes(), "秘密鍵")?);
        let mut public_key = [0; 32];
        libcrux_ed25519::secret_to_public(&mut public_key, &secret);
        Ok(SigningKeyPair {
            secret_key: seed.clone(),
            public_key: public_key.to_vec(),
        })
    }

    fn sign(secret_key: &SecretBytes, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        let secret = Zeroizing::new(array::<32>(secret_key.as_bytes(), "秘密鍵")?);
        libcrux_ed25519::sign(message, &secret)
            .map(|signature| signature.to_vec())
            .map_err(|_| CryptoError::Sign)
    }

    fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        libcrux_ed25519::verify(
            message,
            &array::<32>(public_key, "公開鍵")?,
            &array::<64>(signature, "署名")?,
        )
        .map_err(|_| CryptoError::Verify)
    }

    fn aead_encrypt(
        algorithm: AeadAlgorithm,
        key: &SecretBytes,
        nonce: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CryptoError> {
        match algorithm {
            AeadAlgorithm::Aes256Gcm => encrypt::<AesGcm256>(key, nonce, aad, plaintext),
            AeadAlgorithm::ChaCha20Poly1305 => {
                encrypt::<ChaCha20Poly1305>(key, nonce, aad, plaintext)
            }
            AeadAlgorithm::XChaCha20Poly1305 => {
                encrypt::<XChaCha20Poly1305>(key, nonce, aad, plaintext)
            }
        }
    }

    fn aead_decrypt(
        algorithm: AeadAlgorithm,
        key: &SecretBytes,
        nonce: &[u8],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<SecretBytes, CryptoError> {
        match algorithm {
            AeadAlgorithm::Aes256Gcm => decrypt::<AesGcm256>(key, nonce, aad, ciphertext),
            AeadAlgorithm::ChaCha20Poly1305 => {
                decrypt::<ChaCha20Poly1305>(key, nonce, aad, ciphertext)
            }
            AeadAlgorithm::XChaCha20Poly1305 => {
                decrypt::<XChaCha20Poly1305>(key, nonce, aad, ciphertext)
            }
        }
    }

    fn hkdf_sha256(
        salt: &[u8],
        ikm: &SecretBytes,
        info: &[u8],
        len: usize,
    ) -> Result<SecretBytes, CryptoError> {
        let mut okm = hkdf_sha256_output(len)?;
        libcrux_hkdf::hkdf(
            libcrux_hkdf::Algorithm::Sha256,
            &mut okm,
            salt,
            ikm.as_bytes(),
            info,
        )
        .map_err(|_| CryptoError::Derive)?;
        Ok(SecretBytes(okm))
    }
}
