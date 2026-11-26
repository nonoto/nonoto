//! 同期のレコードに使う暗号の部品と、その組 (方式)。
//!
//! 暗号のライブラリの型は公開APIに出さず、鍵はこのモジュールの型で包む。
//! 部品はライブラリの入口だけを使い、組み合わせ方は方式ごとに [`Suite`] が決める。

use std::fmt;

use chacha20::ChaCha20Rng;
use chacha20::rand_core::SeedableRng;
use chacha20poly1305::XChaCha20Poly1305;
use chacha20poly1305::aead::{Aead, KeyInit, Nonce, Payload};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use hkdf::Hkdf;
use hpke::aead::{AeadTag, ChaCha20Poly1305 as HpkeChaCha20Poly1305};
use hpke::kdf::HkdfSha256;
use hpke::kem::XWing;
use hpke::{Deserializable, Kem, OpModeR, OpModeS, Serializable};
use sha2::Sha256;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

use crate::error::Error;

/// トピックの鍵の長さ (バイト)。
pub(crate) const TOPIC_KEY_LEN: usize = 32;

/// 中身の鍵の長さ (バイト)。XChaCha20-Poly1305の鍵の長さ。
const CONTENT_KEY_LEN: usize = 32;

/// 書き込みの鍵と利用者の署名の鍵 (Ed25519) の種と公開鍵の長さ (バイト)。
pub(crate) const ED25519_KEY_LEN: usize = 32;

/// 書き込みの署名 (Ed25519) の長さ (バイト)。
pub(crate) const SIGNATURE_LEN: usize = 64;

/// 中身の暗号化のnonceの長さ (バイト)。XChaCha20-Poly1305のnonceの長さ。
pub(crate) const NONCE_LEN: usize = 24;

/// 中身の暗号化で増える認証タグの長さ (バイト)。
pub(crate) const TAG_LEN: usize = 16;

/// 中身の鍵を導くときの、用途の区切りの文字列。
const CONTENT_KEY_DOMAIN: &[u8] = b"nonoto/record/content-key\0";

/// 暗号の部品の組。封筒の方式のIDで指す (ADR #0010)。
///
/// 部品ごとのIDは持たず、組の中身を替えるときは新しい方式を足す。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Suite {
    /// 鍵の包みにX-WingのHPKE (HKDF-SHA256・ChaCha20-Poly1305)、署名にEd25519、
    /// 中身の暗号化にXChaCha20-Poly1305、鍵の導出にHKDF-SHA256、ハッシュにSHA-256を使う。
    XWingEd25519XChaCha20,
    /// 方式と形式の切り替え (ADR #0010) を確かめるテストだけで使う、2つ目の方式。
    ///
    /// 部品は1つ目の方式と同じで、方式のIDだけが違う。
    /// 方式のIDを鍵の導出と追加データに入れるため、1つ目の方式のレコードとは別の鍵で暗号化される。
    /// 本番の方式の一覧 ([`Suite::ALL`]) には入れない。
    #[cfg(any(test, feature = "test-suite"))]
    TestSecond,
}

/// 方式が使う暗号の部品の組。部品が同じ方式は、同じ処理で暗号化と署名をする。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Components {
    XWingEd25519XChaCha20,
}

impl Suite {
    /// クライアントが読める方式の一覧。
    pub const ALL: &[Suite] = &[Suite::XWingEd25519XChaCha20];

    /// 封筒の方式のIDから読める方式。テストの2つ目の方式を含める。
    const KNOWN: &[Suite] = &[
        Suite::XWingEd25519XChaCha20,
        #[cfg(any(test, feature = "test-suite"))]
        Suite::TestSecond,
    ];

    /// 新しいレコードを書くときの方式。
    pub const CURRENT: Suite = Suite::XWingEd25519XChaCha20;

    /// 封筒に置く方式のID。
    pub fn id(self) -> u16 {
        match self {
            Self::XWingEd25519XChaCha20 => 1,
            #[cfg(any(test, feature = "test-suite"))]
            Self::TestSecond => 0xfffe,
        }
    }

    fn components(self) -> Components {
        match self {
            Self::XWingEd25519XChaCha20 => Components::XWingEd25519XChaCha20,
            #[cfg(any(test, feature = "test-suite"))]
            Self::TestSecond => Components::XWingEd25519XChaCha20,
        }
    }

    /// 方式のIDから方式を求める。知らないIDなら [`Error::UnsupportedSuite`] を返す。
    pub fn from_id(id: u16) -> Result<Self, Error> {
        Self::KNOWN
            .iter()
            .copied()
            .find(|suite| suite.id() == id)
            .ok_or(Error::UnsupportedSuite(id))
    }

    /// トピックの鍵から、用途とページのIDを入れて中身の鍵を導く。
    ///
    /// 方式のIDも入れ、同じトピックの鍵から別の方式で同じ中身の鍵が導かれないようにする。
    pub(crate) fn derive_content_key(
        self,
        topic_key: &TopicKey,
        purpose: u8,
        page_id: &[u8],
    ) -> ContentKey {
        match self.components() {
            Components::XWingEd25519XChaCha20 => {
                let mut info = Vec::with_capacity(CONTENT_KEY_DOMAIN.len() + 3 + page_id.len());
                info.extend_from_slice(CONTENT_KEY_DOMAIN);
                info.extend_from_slice(&self.id().to_be_bytes());
                info.push(purpose);
                info.extend_from_slice(page_id);
                let mut key = ContentKey([0; CONTENT_KEY_LEN]);
                // 中間鍵 (PRK) からは、同じトピックのすべての中身の鍵を導けるため、受け取ってすぐに消す。
                let (mut prk, hkdf) = Hkdf::<Sha256>::extract(None, &topic_key.0);
                prk.as_mut_slice().zeroize();
                hkdf.expand(&info, &mut key.0).expect(
                    "中身の鍵は32バイトで、HKDF-SHA256で導ける長さ (255×32バイト) に収まる",
                );
                key
            }
        }
    }

    /// 中身を暗号化する。返す暗号文の末尾に認証タグを含む。
    pub(crate) fn encrypt(
        self,
        key: &ContentKey,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Vec<u8>, Error> {
        match self.components() {
            Components::XWingEd25519XChaCha20 => {
                let cipher =
                    XChaCha20Poly1305::new_from_slice(&key.0).map_err(|_| Error::EncryptRecord)?;
                let nonce = Nonce::<XChaCha20Poly1305>::from(*nonce);
                cipher
                    .encrypt(
                        &nonce,
                        Payload {
                            msg: plaintext,
                            aad,
                        },
                    )
                    .map_err(|_| Error::EncryptRecord)
            }
        }
    }

    /// 中身を復号する。鍵・nonce・追加データ・暗号文のどれかが違えば [`Error::DecryptRecord`] を返す。
    pub(crate) fn decrypt(
        self,
        key: &ContentKey,
        nonce: &[u8; NONCE_LEN],
        aad: &[u8],
        ciphertext: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        match self.components() {
            Components::XWingEd25519XChaCha20 => {
                let cipher =
                    XChaCha20Poly1305::new_from_slice(&key.0).map_err(|_| Error::DecryptRecord)?;
                let nonce = Nonce::<XChaCha20Poly1305>::from(*nonce);
                cipher
                    .decrypt(
                        &nonce,
                        Payload {
                            msg: ciphertext,
                            aad,
                        },
                    )
                    .map(Zeroizing::new)
                    .map_err(|_| Error::DecryptRecord)
            }
        }
    }
}

/// 世代ごとのトピックの鍵。中身の鍵を導く元にする。
///
/// ログに出さないよう、`Debug` では中身を表示しない。
/// 使い終わったら、メモリ上の値を消す。
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct TopicKey([u8; TOPIC_KEY_LEN]);

impl TopicKey {
    /// 乱数から新しいトピックの鍵を作る。
    pub fn generate() -> Result<Self, Error> {
        let mut key = Self([0; TOPIC_KEY_LEN]);
        getrandom::fill(&mut key.0).map_err(Error::Random)?;
        Ok(key)
    }

    /// 借用したバイト列から、破棄時に消去されるトピックの鍵へ写す。
    pub(crate) fn from_bytes(bytes: &[u8; TOPIC_KEY_LEN]) -> Self {
        let mut key = Self([0; TOPIC_KEY_LEN]);
        key.0.copy_from_slice(bytes);
        key
    }

    /// 世代の記録の包みに入れる、鍵のバイト列を借りる。
    pub(crate) fn as_bytes(&self) -> &[u8; TOPIC_KEY_LEN] {
        &self.0
    }
}

impl fmt::Debug for TopicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("TopicKey(..)")
    }
}

/// トピックの鍵から導いた、用途とページごとの中身の鍵。
#[derive(Zeroize, ZeroizeOnDrop)]
pub(crate) struct ContentKey([u8; CONTENT_KEY_LEN]);

/// 書き込みの鍵の秘密鍵。書く先 (本文か編集提案) ごと・世代ごとに作り、同じ役割のメンバーで共有する (ADR #0009)。
///
/// ログに出さないよう、`Debug` では中身を表示しない。
/// 使い終わったら、メモリ上の値を消す (ライブラリの鍵の型がDropで消す)。
#[derive(Clone)]
pub struct WriteSigningKey(SigningKey);

impl WriteSigningKey {
    /// 乱数から新しい書き込みの鍵を作る。
    pub fn generate() -> Result<Self, Error> {
        let mut seed = Zeroizing::new([0; ED25519_KEY_LEN]);
        getrandom::fill(seed.as_mut()).map_err(Error::Random)?;
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    /// 書き込みの公開鍵を返す。
    pub fn verifying_key(&self) -> WriteVerifyingKey {
        WriteVerifyingKey(self.0.verifying_key())
    }

    /// 種 (RFC 8032の秘密鍵) から書き込みの鍵を作る。世代の記録の包みから取り出すときに使う。
    pub(crate) fn from_seed(seed: &[u8; ED25519_KEY_LEN]) -> Self {
        Self(SigningKey::from_bytes(seed))
    }

    /// 世代の記録の包みに入れる種を借りる。
    pub(crate) fn seed(&self) -> &[u8; ED25519_KEY_LEN] {
        self.0.as_bytes()
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        self.0.sign(message).to_bytes()
    }
}

impl fmt::Debug for WriteSigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WriteSigningKey(..)")
    }
}

/// 書き込みの鍵の公開鍵。世代の記録に入れ、サーバーにも見せる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WriteVerifyingKey(VerifyingKey);

impl WriteVerifyingKey {
    /// 公開鍵のバイト列から作る。点として読めなければ [`Error::InvalidPublicKey`] を返す。
    pub fn from_bytes(bytes: &[u8; ED25519_KEY_LEN]) -> Result<Self, Error> {
        VerifyingKey::from_bytes(bytes)
            .map(Self)
            .map_err(|_| Error::InvalidPublicKey)
    }

    /// 世代の記録に入れる、書き込みの公開鍵のバイト列を返す。
    pub fn to_bytes(self) -> [u8; ED25519_KEY_LEN] {
        self.0.to_bytes()
    }

    /// 署名を確かめる。
    ///
    /// 位数の小さい公開鍵とRを拒む厳密な確かめ (`verify_strict`) にし、
    /// どの端末とサーバーでも同じ範囲の署名だけを受け付ける。
    pub(crate) fn verify(
        self,
        message: &[u8],
        signature: &[u8; SIGNATURE_LEN],
    ) -> Result<(), Error> {
        self.0
            .verify_strict(message, &Signature::from_bytes(signature))
            .map_err(|_| Error::InvalidWriteSignature)
    }
}

/// 利用者の署名の鍵の秘密鍵。利用者のアカウントごとに持ち、端末の間で共有する。
///
/// 世代の記録の作り手の署名と、更新の著者の署名に使う。
/// ログに出さないよう、`Debug` では中身を表示しない。
#[derive(Clone)]
pub struct UserSigningKey(SigningKey);

impl UserSigningKey {
    /// 乱数から新しい署名の鍵を作る。
    pub fn generate() -> Result<Self, Error> {
        let mut seed = Zeroizing::new([0; ED25519_KEY_LEN]);
        getrandom::fill(seed.as_mut()).map_err(Error::Random)?;
        Ok(Self(SigningKey::from_bytes(&seed)))
    }

    /// 署名の公開鍵を返す。
    pub fn verifying_key(&self) -> UserVerifyingKey {
        UserVerifyingKey(self.0.verifying_key())
    }

    pub(crate) fn sign(&self, message: &[u8]) -> [u8; SIGNATURE_LEN] {
        self.0.sign(message).to_bytes()
    }
}

impl fmt::Debug for UserSigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("UserSigningKey(..)")
    }
}

/// 利用者の署名の公開鍵。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UserVerifyingKey(VerifyingKey);

impl UserVerifyingKey {
    /// 公開鍵のバイト列から作る。点として読めなければ [`Error::InvalidPublicKey`] を返す。
    pub fn from_bytes(bytes: &[u8; ED25519_KEY_LEN]) -> Result<Self, Error> {
        VerifyingKey::from_bytes(bytes)
            .map(Self)
            .map_err(|_| Error::InvalidPublicKey)
    }

    /// 利用者の署名の公開鍵をバイト列にする。
    pub fn to_bytes(self) -> [u8; ED25519_KEY_LEN] {
        self.0.to_bytes()
    }

    /// 同じ点を表す公開鍵の重複を確かめるため、点を正規の符号化で書き出す。
    pub(crate) fn canonical_bytes(self) -> [u8; ED25519_KEY_LEN] {
        self.0.to_edwards().compress().to_bytes()
    }

    /// 更新の著者の署名を、書き込みの署名と同じ厳密な確かめで確かめる。
    pub(crate) fn verify_author(
        self,
        message: &[u8],
        signature: &[u8; SIGNATURE_LEN],
    ) -> Result<(), Error> {
        self.0
            .verify_strict(message, &Signature::from_bytes(signature))
            .map_err(|_| Error::InvalidAuthorSignature)
    }

    /// 世代の記録の作り手の署名を、書き込みの署名と同じ厳密な確かめで確かめる。
    pub(crate) fn verify_creator(
        self,
        message: &[u8],
        signature: &[u8; SIGNATURE_LEN],
    ) -> Result<(), Error> {
        self.0
            .verify_strict(message, &Signature::from_bytes(signature))
            .map_err(|_| Error::InvalidCreatorSignature)
    }
}

/// 鍵を包むための鍵 (HPKEの受け手の鍵) の秘密鍵。利用者のアカウントごとに持つ。
///
/// ログに出さないよう、`Debug` では中身を表示しない。
/// 使い終わったら、メモリ上の値を消す。
#[derive(Clone)]
pub struct WrapSecretKey(Zeroizing<Vec<u8>>);

impl fmt::Debug for WrapSecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WrapSecretKey(..)")
    }
}

/// 鍵を包むための鍵の公開鍵。
#[derive(Clone, PartialEq, Eq)]
pub struct WrapPublicKey(Vec<u8>);

impl WrapPublicKey {
    /// 公開鍵のバイト列を借りる。
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for WrapPublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "WrapPublicKey({}バイト)", self.0.len())
    }
}

/// HPKEで包んだ鍵。カプセルと暗号文の組。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Wrapped {
    pub(crate) enc: Vec<u8>,
    pub(crate) ciphertext: Vec<u8>,
}

impl Suite {
    /// 鍵を包むための鍵の組を、OSの乱数から作る。
    pub fn generate_wrap_keys(self) -> Result<(WrapSecretKey, WrapPublicKey), Error> {
        match self.components() {
            Components::XWingEd25519XChaCha20 => {
                // 乱数を得られないことを、パニックさせずにエラーで返すため、種はここで取る。
                let mut ikm = Zeroizing::new(vec![0; <XWing as Kem>::PrivateKey::size()]);
                getrandom::fill(&mut ikm).map_err(Error::Random)?;
                let (secret, public) = XWing::derive_keypair(&ikm);
                let mut secret_bytes = Zeroizing::new(vec![0; <XWing as Kem>::PrivateKey::size()]);
                secret.write_exact(&mut secret_bytes);
                Ok((
                    WrapSecretKey(secret_bytes),
                    WrapPublicKey(public.to_bytes().to_vec()),
                ))
            }
        }
    }

    /// 包みを開けずに構造を読むときに必要な、カプセルと認証タグの長さ。
    pub(crate) fn wrap_lengths(self) -> (usize, usize) {
        match self.components() {
            Components::XWingEd25519XChaCha20 => (
                <XWing as Kem>::EncappedKey::size(),
                AeadTag::<HpkeChaCha20Poly1305>::size(),
            ),
        }
    }

    /// 受け手の公開鍵で、平文をHPKE (基本の形) で包む。
    pub(crate) fn wrap(
        self,
        public_key: &WrapPublicKey,
        info: &[u8],
        aad: &[u8],
        plaintext: &[u8],
    ) -> Result<Wrapped, Error> {
        match self.components() {
            Components::XWingEd25519XChaCha20 => {
                let public_key = <XWing as Kem>::PublicKey::from_bytes(&public_key.0)
                    .map_err(|_| Error::InvalidPublicKey)?;
                // `single_shot_seal_with_rng` は失敗を返せない乱数生成器を受け取るため、
                // OSの乱数を得られないことをここでエラーにしてから、種を入れた生成器を渡す。
                let mut rng =
                    ChaCha20Rng::try_from_rng(&mut getrandom::SysRng).map_err(Error::Random)?;
                let (enc, ciphertext) =
                    hpke::single_shot_seal_with_rng::<HpkeChaCha20Poly1305, HkdfSha256, XWing>(
                        &OpModeS::Base,
                        &public_key,
                        info,
                        plaintext,
                        aad,
                        &mut rng,
                    )
                    .map_err(|_| Error::EncryptRecord)?;
                Ok(Wrapped {
                    enc: enc.to_bytes().to_vec(),
                    ciphertext,
                })
            }
        }
    }

    /// 受け手の秘密鍵で、HPKEで包んだ平文を開く。
    ///
    /// 鍵・カプセル・info・追加データ・暗号文のどれかが違えば [`Error::DecryptRecord`] を返す。
    pub(crate) fn unwrap(
        self,
        secret_key: &WrapSecretKey,
        wrapped: &Wrapped,
        info: &[u8],
        aad: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, Error> {
        match self.components() {
            Components::XWingEd25519XChaCha20 => {
                let secret_key = <XWing as Kem>::PrivateKey::from_bytes(&secret_key.0)
                    .map_err(|_| Error::DecryptRecord)?;
                let enc = <XWing as Kem>::EncappedKey::from_bytes(&wrapped.enc)
                    .map_err(|_| Error::DecryptRecord)?;
                hpke::single_shot_open::<HpkeChaCha20Poly1305, HkdfSha256, XWing>(
                    &OpModeR::Base,
                    &secret_key,
                    &enc,
                    info,
                    &wrapped.ciphertext,
                    aad,
                )
                .map(Zeroizing::new)
                .map_err(|_| Error::DecryptRecord)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 封筒に置く値から方式を求め直せる() {
        for suite in Suite::ALL {
            assert_eq!(
                Suite::from_id(suite.id()).expect("一覧の方式のIDから方式を求められなかった"),
                *suite,
                "方式のIDから別の方式が求まった",
            );
        }
    }

    #[test]
    fn 知らない方式の値を拒む() {
        assert!(
            matches!(Suite::from_id(0), Err(Error::UnsupportedSuite(0))),
            "知らない方式のIDを受け付けた",
        );
    }

    #[test]
    fn 用途かページが違うと別の中身の鍵になる() {
        let topic_key = TopicKey::generate().expect("トピックの鍵を作れなかった");
        let derive = |purpose, page: &[u8]| {
            Suite::CURRENT
                .derive_content_key(&topic_key, purpose, page)
                .0
        };

        let base = derive(1, &[1; 16]);
        assert_ne!(base, derive(2, &[1; 16]), "用途が違っても同じ鍵になった");
        assert_ne!(
            base,
            derive(1, &[2; 16]),
            "ページのIDが違っても同じ鍵になった"
        );
        assert_eq!(base, derive(1, &[1; 16]), "同じ入力から別の鍵が導かれた");
    }

    #[test]
    fn 方式1のログの中身の鍵を同じ入力から導ける() {
        let topic_key = TopicKey([0x5a; TOPIC_KEY_LEN]);
        let key =
            Suite::XWingEd25519XChaCha20.derive_content_key(&topic_key, 1, b"fedcba9876543210");
        let expected = [
            0xd6, 0xf7, 0xaa, 0x9d, 0x3e, 0xaa, 0xb1, 0x6b, 0x44, 0x09, 0x1a, 0x7e, 0x8b, 0xca,
            0x48, 0xaf, 0x70, 0x59, 0x60, 0xf5, 0x0f, 0xc4, 0x42, 0x89, 0x04, 0x40, 0x80, 0x4d,
            0xb2, 0xdc, 0x50, 0xf3,
        ];
        assert!(key.0 == expected, "方式1で導いた中身の鍵が変わった");
    }

    #[test]
    fn 鍵のデバッグ表示に中身を出さない() {
        let topic_key = TopicKey::generate().expect("トピックの鍵を作れなかった");
        let write_key = WriteSigningKey::generate().expect("書き込みの鍵を作れなかった");

        assert_eq!(
            format!("{topic_key:?}"),
            "TopicKey(..)",
            "トピックの鍵を表示した"
        );
        assert_eq!(
            format!("{write_key:?}"),
            "WriteSigningKey(..)",
            "書き込みの鍵を表示した",
        );
    }
}
