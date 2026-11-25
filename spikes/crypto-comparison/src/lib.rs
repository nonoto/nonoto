//! 暗号の部品のライブラリを、系統ごとに同じ操作で比べるための実装。
//!
//! 系統ごとのアダプターが [`CryptoFamily`] を実装する。
//! テストと計測は [`CryptoFamily`] だけを使って書き、すべての系統に同じ操作を当てる。
//! 鍵・暗号文・署名はバイト列で受け渡し、ライブラリの型をアダプターの外に出さない。

#[cfg(not(any(feature = "rustcrypto", feature = "libcrux")))]
compile_error!("系統のフィーチャー (rustcrypto・libcrux) を1つ以上有効にする");

pub mod conformance;
#[cfg(feature = "libcrux")]
mod libcrux_adapter;
pub mod measure;
#[cfg(feature = "rustcrypto")]
mod rustcrypto_adapter;

use std::fmt;

use zeroize::Zeroizing;

#[cfg(feature = "libcrux")]
pub use crate::libcrux_adapter::Libcrux;
#[cfg(feature = "rustcrypto")]
pub use crate::rustcrypto_adapter::RustCrypto;

/// アダプターの操作の失敗。
///
/// 鍵・平文が含まれないよう、ライブラリのエラーの中身は持たず、種類だけを持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum CryptoError {
    /// 乱数を取得できなかった。
    #[error("乱数を取得できなかった")]
    Random,
    /// 鍵・nonce・暗号文などの長さか形が、方式に合わない。
    #[error("{0}の長さか形が方式に合わない")]
    InvalidInput(&'static str),
    /// HPKEで封じられなかった。
    #[error("HPKEで封じられなかった")]
    Seal,
    /// HPKEの暗号文を開けなかった (鍵・`info`・追加データ・暗号文のいずれかが合わない)。
    #[error("HPKEの暗号文を開けなかった")]
    Open,
    /// 署名を作れなかった。
    #[error("署名を作れなかった")]
    Sign,
    /// 署名が合わない。
    #[error("署名が合わない")]
    Verify,
    /// AEADで暗号化できなかった。
    #[error("AEADで暗号化できなかった")]
    Encrypt,
    /// AEADの暗号文を復号できなかった (鍵・nonce・追加データ・暗号文のいずれかが合わない)。
    #[error("AEADの暗号文を復号できなかった")]
    Decrypt,
    /// HKDFで指定の長さの鍵を導けなかった。
    #[error("HKDFで鍵を導けなかった")]
    Derive,
    /// 計測の標本を指定された回数分確保できなかった。
    #[error("指定された計測回数の標本を確保できなかった")]
    SampleCapacity,
}

/// 使い終えたら消す、秘密の値のバイト列。デバッグ表示では長さだけを出す。
#[derive(Clone, PartialEq, Eq)]
pub struct SecretBytes(Zeroizing<Vec<u8>>);

impl SecretBytes {
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(Zeroizing::new(bytes))
    }

    pub fn from_slice(bytes: &[u8]) -> Self {
        Self::new(bytes.to_vec())
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Debug for SecretBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBytes({}バイト)", self.0.len())
    }
}

/// HPKEで包むための鍵の組。
#[derive(Debug, Clone)]
pub struct WrapKeyPair {
    pub secret_key: SecretBytes,
    pub public_key: Vec<u8>,
}

/// 署名の鍵の組。秘密鍵はRFC 8032の32バイトの種で持つ。
#[derive(Debug, Clone)]
pub struct SigningKeyPair {
    pub secret_key: SecretBytes,
    pub public_key: Vec<u8>,
}

/// HPKEで封じた結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sealed {
    /// KEMのカプセル (RFC 9180の `enc`)。
    pub enc: Vec<u8>,
    /// AEADの暗号文とタグ。
    pub ciphertext: Vec<u8>,
}

/// HPKEのKEM。
///
/// KDFはHKDF-SHA256、AEADはChaCha20-Poly1305に固定する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HpkeKem {
    /// X25519とML-KEM-768のハイブリッド (X-Wing)。鍵の階層で使う候補。
    XWing,
    /// DHKEM(X25519, HKDF-SHA256)。RFC 9180の試験ベクトルで、HPKEの鍵の予定を確かめるために使う。
    DhKemX25519,
}

impl HpkeKem {
    pub const ALL: [Self; 2] = [Self::XWing, Self::DhKemX25519];

    /// RFC 9180とdraft-ietf-hpke-pqのKEMのID。
    pub fn id(self) -> u16 {
        match self {
            Self::XWing => 0x647a,
            Self::DhKemX25519 => 0x0020,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::XWing => "X-Wing",
            Self::DhKemX25519 => "DHKEM(X25519)",
        }
    }
}

/// AEADの方式。いずれも256ビットの鍵を使う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AeadAlgorithm {
    Aes256Gcm,
    ChaCha20Poly1305,
    XChaCha20Poly1305,
}

impl AeadAlgorithm {
    pub const ALL: [Self; 3] = [
        Self::Aes256Gcm,
        Self::ChaCha20Poly1305,
        Self::XChaCha20Poly1305,
    ];

    pub const KEY_LEN: usize = 32;
    pub const TAG_LEN: usize = 16;

    pub fn nonce_len(self) -> usize {
        match self {
            Self::Aes256Gcm | Self::ChaCha20Poly1305 => 12,
            Self::XChaCha20Poly1305 => 24,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::Aes256Gcm => "AES-256-GCM",
            Self::ChaCha20Poly1305 => "ChaCha20-Poly1305",
            Self::XChaCha20Poly1305 => "XChaCha20-Poly1305",
        }
    }

    /// 乱数のnonceで1つの鍵に使える回数の目安 (log2)。
    ///
    /// nonceが重なる確率を2の-32乗以下に抑える回数で、NIST SP 800-38Dの8.3節の考え方を、
    /// nonceの長さ (ビット) から誕生日の上限で見積もる (`(nonceのビット数 - 32) / 2`)。
    pub fn random_nonce_limit_log2(self) -> u32 {
        let bits = u32::try_from(self.nonce_len() * 8).expect("nonceのビット数はu32に収まる");
        (bits - 32) / 2
    }
}

/// 暗号の部品の1つの系統。
///
/// 操作はすべて関連関数にし、系統の間で鍵と暗号文をバイト列のまま受け渡せるようにする。
pub trait CryptoFamily {
    /// 結果の出力に使う系統の名前。
    const NAME: &'static str;

    /// Ed25519の確かめに使う入口の説明 (結果の出力に使う)。
    const VERIFY_ENTRY: &'static str;

    /// 包むための鍵の組を、OSの乱数で作る。
    fn generate_wrap_keypair(kem: HpkeKem) -> Result<WrapKeyPair, CryptoError>;

    /// 包むための鍵の組を、RFC 9180の `DeriveKeyPair` で `ikm` から導く。
    fn derive_wrap_keypair(kem: HpkeKem, ikm: &[u8]) -> Result<WrapKeyPair, CryptoError>;

    /// HPKEのbase modeで、公開鍵に向けて1回封じる。
    fn seal(
        kem: HpkeKem,
        public_key: &[u8],
        info: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Sealed, CryptoError>;

    /// HPKEのbase modeで、秘密鍵で開く。
    fn open(
        kem: HpkeKem,
        secret_key: &SecretBytes,
        enc: &[u8],
        info: &[u8],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<SecretBytes, CryptoError>;

    /// Ed25519の32バイトの種から、署名の鍵の組を作る。
    fn signing_keypair_from_seed(seed: &SecretBytes) -> Result<SigningKeyPair, CryptoError>;

    /// Ed25519で署名する。
    fn sign(secret_key: &SecretBytes, message: &[u8]) -> Result<Vec<u8>, CryptoError>;

    /// Ed25519の署名を確かめる。系統にある、もっとも厳密な確かめの入口を使う。
    fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError>;

    /// AEADで暗号化し、暗号文の後ろにタグを付けて返す。
    fn aead_encrypt(
        algorithm: AeadAlgorithm,
        key: &SecretBytes,
        nonce: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, CryptoError>;

    /// タグを後ろに付けたAEADの暗号文を復号する。
    fn aead_decrypt(
        algorithm: AeadAlgorithm,
        key: &SecretBytes,
        nonce: &[u8],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<SecretBytes, CryptoError>;

    /// HKDF-SHA256で `len` バイトの鍵を導く。
    fn hkdf_sha256(
        salt: &[u8],
        ikm: &SecretBytes,
        info: &[u8],
        len: usize,
    ) -> Result<SecretBytes, CryptoError>;

    /// 署名の鍵の組を、OSの乱数で作る。
    fn generate_signing_keypair() -> Result<SigningKeyPair, CryptoError> {
        Self::signing_keypair_from_seed(&random_secret(32)?)
    }
}

/// OSの乱数で、`len` バイトの秘密の値を作る。
pub fn random_secret(len: usize) -> Result<SecretBytes, CryptoError> {
    let mut bytes = Zeroizing::new(vec![0; len]);
    getrandom::fill(&mut bytes).map_err(|_| CryptoError::Random)?;
    Ok(SecretBytes(bytes))
}

/// HKDF-SHA256の出力を、RFC 5869の上限内で確保する。
fn hkdf_sha256_output(len: usize) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    const MAX_OUTPUT_LEN: usize = 255 * 32;
    if len > MAX_OUTPUT_LEN {
        return Err(CryptoError::Derive);
    }

    Ok(Zeroizing::new(vec![0; len]))
}

/// 長さを確かめて、固定長の配列に写す。
pub(crate) fn array<const N: usize>(
    bytes: &[u8],
    what: &'static str,
) -> Result<[u8; N], CryptoError> {
    bytes
        .try_into()
        .map_err(|_| CryptoError::InvalidInput(what))
}
