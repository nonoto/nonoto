//! RustCryptoの系統 (`hpke`・`ed25519-dalek`・`aes-gcm`・`chacha20poly1305`・`hkdf`) のアダプター。

use aes_gcm::Aes256Gcm;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, Nonce, Payload};
use chacha20::{ChaCha20Rng, rand_core::SeedableRng};
use chacha20poly1305::{ChaCha20Poly1305, XChaCha20Poly1305};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use hpke::aead::ChaCha20Poly1305 as HpkeChaCha20Poly1305;
use hpke::kdf::HkdfSha256;
use hpke::kem::{X25519HkdfSha256, XWing};
use hpke::{Deserializable, Kem, OpModeR, OpModeS, Serializable};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::{
    AeadAlgorithm, CryptoError, CryptoFamily, HpkeKem, Sealed, SecretBytes, SigningKeyPair,
    WrapKeyPair, array, hkdf_sha256_output, random_secret,
};

/// RustCryptoの系統。
#[derive(Debug, Clone, Copy)]
pub struct RustCrypto;

fn derive<K: Kem>(ikm: &[u8]) -> WrapKeyPair {
    let (secret_key, public_key) = K::derive_keypair(ikm);
    let mut secret_bytes = Zeroizing::new(vec![0; K::PrivateKey::size()]);
    secret_key.write_exact(&mut secret_bytes);
    WrapKeyPair {
        secret_key: SecretBytes(secret_bytes),
        public_key: public_key.to_bytes().to_vec(),
    }
}

/// OSの乱数で `Nsk` バイトの種を取り、そこから鍵の組を導く。
///
/// 乱数を取れなかったことを、パニックさせずに `CryptoError::Random` で返すため、種はここで取る。
fn generate<K: Kem>() -> Result<WrapKeyPair, CryptoError> {
    let ikm = random_secret(K::PrivateKey::size())?;
    Ok(derive::<K>(ikm.as_bytes()))
}

fn seal<K: Kem>(
    public_key: &[u8],
    info: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Sealed, CryptoError> {
    let public_key =
        K::PublicKey::from_bytes(public_key).map_err(|_| CryptoError::InvalidInput("公開鍵"))?;
    // `single_shot_seal_with_rng` は失敗を返せない乱数生成器を受け取るため、
    // OSの乱数の取得の失敗をここで `CryptoError::Random` にしてから、種を入れた生成器を渡す。
    let mut rng =
        ChaCha20Rng::try_from_rng(&mut getrandom::SysRng).map_err(|_| CryptoError::Random)?;
    let (enc, ciphertext) = hpke::single_shot_seal_with_rng::<HpkeChaCha20Poly1305, HkdfSha256, K>(
        &OpModeS::Base,
        &public_key,
        info,
        plaintext,
        aad,
        &mut rng,
    )
    .map_err(|_| CryptoError::Seal)?;
    Ok(Sealed {
        enc: enc.to_bytes().to_vec(),
        ciphertext,
    })
}

fn open<K: Kem>(
    secret_key: &SecretBytes,
    enc: &[u8],
    info: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<SecretBytes, CryptoError> {
    let secret_key = K::PrivateKey::from_bytes(secret_key.as_bytes())
        .map_err(|_| CryptoError::InvalidInput("秘密鍵"))?;
    let enc = K::EncappedKey::from_bytes(enc).map_err(|_| CryptoError::InvalidInput("カプセル"))?;
    hpke::single_shot_open::<HpkeChaCha20Poly1305, HkdfSha256, K>(
        &OpModeR::Base,
        &secret_key,
        &enc,
        info,
        ciphertext,
        aad,
    )
    .map(SecretBytes::new)
    .map_err(|_| CryptoError::Open)
}

fn encrypt<C: Aead + KeyInit>(
    key: &SecretBytes,
    nonce: &[u8],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>, CryptoError> {
    let cipher = C::new_from_slice(key.as_bytes()).map_err(|_| CryptoError::InvalidInput("鍵"))?;
    let nonce = Nonce::<C>::try_from(nonce).map_err(|_| CryptoError::InvalidInput("nonce"))?;
    cipher
        .encrypt(
            &nonce,
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| CryptoError::Encrypt)
}

fn decrypt<C: Aead + AeadCore + KeyInit>(
    key: &SecretBytes,
    nonce: &[u8],
    aad: &[u8],
    ciphertext: &[u8],
) -> Result<SecretBytes, CryptoError> {
    let cipher = C::new_from_slice(key.as_bytes()).map_err(|_| CryptoError::InvalidInput("鍵"))?;
    let nonce = Nonce::<C>::try_from(nonce).map_err(|_| CryptoError::InvalidInput("nonce"))?;
    cipher
        .decrypt(
            &nonce,
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map(SecretBytes::new)
        .map_err(|_| CryptoError::Decrypt)
}

fn signing_key(secret_key: &SecretBytes) -> Result<SigningKey, CryptoError> {
    let seed = Zeroizing::new(array::<32>(secret_key.as_bytes(), "秘密鍵")?);
    Ok(SigningKey::from_bytes(&seed))
}

impl CryptoFamily for RustCrypto {
    const NAME: &'static str = "RustCrypto";
    const VERIFY_ENTRY: &'static str = "ed25519-dalekの `verify_strict`";

    fn generate_wrap_keypair(kem: HpkeKem) -> Result<WrapKeyPair, CryptoError> {
        match kem {
            HpkeKem::XWing => generate::<XWing>(),
            HpkeKem::DhKemX25519 => generate::<X25519HkdfSha256>(),
        }
    }

    fn derive_wrap_keypair(kem: HpkeKem, ikm: &[u8]) -> Result<WrapKeyPair, CryptoError> {
        Ok(match kem {
            HpkeKem::XWing => derive::<XWing>(ikm),
            HpkeKem::DhKemX25519 => derive::<X25519HkdfSha256>(ikm),
        })
    }

    fn seal(
        kem: HpkeKem,
        public_key: &[u8],
        info: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Sealed, CryptoError> {
        match kem {
            HpkeKem::XWing => seal::<XWing>(public_key, info, aad, plaintext),
            HpkeKem::DhKemX25519 => seal::<X25519HkdfSha256>(public_key, info, aad, plaintext),
        }
    }

    fn open(
        kem: HpkeKem,
        secret_key: &SecretBytes,
        enc: &[u8],
        info: &[u8],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<SecretBytes, CryptoError> {
        match kem {
            HpkeKem::XWing => open::<XWing>(secret_key, enc, info, aad, ciphertext),
            HpkeKem::DhKemX25519 => {
                open::<X25519HkdfSha256>(secret_key, enc, info, aad, ciphertext)
            }
        }
    }

    fn signing_keypair_from_seed(seed: &SecretBytes) -> Result<SigningKeyPair, CryptoError> {
        let key = signing_key(seed)?;
        Ok(SigningKeyPair {
            secret_key: seed.clone(),
            public_key: key.verifying_key().to_bytes().to_vec(),
        })
    }

    fn sign(secret_key: &SecretBytes, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
        Ok(signing_key(secret_key)?.sign(message).to_bytes().to_vec())
    }

    fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
        let public_key = VerifyingKey::from_bytes(&array::<32>(public_key, "公開鍵")?)
            .map_err(|_| CryptoError::Verify)?;
        let signature = Signature::from_bytes(&array::<64>(signature, "署名")?);
        public_key
            .verify_strict(message, &signature)
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
            AeadAlgorithm::Aes256Gcm => encrypt::<Aes256Gcm>(key, nonce, aad, plaintext),
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
            AeadAlgorithm::Aes256Gcm => decrypt::<Aes256Gcm>(key, nonce, aad, ciphertext),
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
        let hkdf = Hkdf::<Sha256>::new(Some(salt), ikm.as_bytes());
        hkdf.expand(info, &mut okm)
            .map_err(|_| CryptoError::Derive)?;
        Ok(SecretBytes(okm))
    }
}
