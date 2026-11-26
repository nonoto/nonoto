//! 必須の条件 (規格の試験ベクトル・往復・書き換えの拒否) を、系統に当てて確かめる。
//!
//! `src/vectors/` のファイルは、次の出典から、使う部分だけを抜き出したもの。
//!
//! - `hpke.json`: RFC 9180の試験ベクトル (github.com/cfrg/draft-irtf-cfrg-hpke の `test-vectors.json`) の
//!   DHKEM(X25519, HKDF-SHA256)・HKDF-SHA256・ChaCha20-Poly1305のbase modeと、
//!   draft-ietf-hpke-pqの試験ベクトル (github.com/hpkewg/hpke-pq の `test-vectors.json`) の
//!   X-Wing・HKDF-SHA256・ChaCha20-Poly1305のbase mode。それぞれ最初の暗号文だけを残した
//! - `ed25519.json`: RFC 8032の7.1節 (1024バイトの例を除く)・Wycheproofの `ed25519_test.json`・
//!   ed25519-speccheckの `cases.json`
//! - `aead.json`: Wycheproofの `aes_gcm_test.json`・`chacha20_poly1305_test.json`・
//!   `xchacha20_poly1305_test.json` のうち、256ビットの鍵と標準のnonceの長さで、平文が256バイト以下のもの
//! - `hkdf.json`: Wycheproofの `hkdf_sha256_test.json`
//!
//! 取り込んだ版とライセンスは `src/vectors/NOTICE.md` に記録している。

use std::sync::LazyLock;

use serde_json::Value;

use crate::{AeadAlgorithm, CryptoFamily, HpkeKem, SecretBytes};

const HPKE: &str = include_str!("vectors/hpke.json");
const ED25519: &str = include_str!("vectors/ed25519.json");
const AEAD: &str = include_str!("vectors/aead.json");
const HKDF: &str = include_str!("vectors/hkdf.json");

fn parse(text: &str) -> Value {
    serde_json::from_str(text).expect("同梱した試験ベクトルはJSONとして読める")
}

static ED25519_VECTORS: LazyLock<Value> = LazyLock::new(|| parse(ED25519));

/// 1つの必須の条件を確かめた結果。
#[derive(Debug)]
pub struct Check {
    /// 条件の名前。
    pub name: &'static str,
    /// 満たさなかったときは、何が合わなかったかの説明。
    pub result: Result<(), String>,
}

/// 1つの条件を確かめる関数。
type CheckFn = fn() -> Result<(), String>;

/// すべての必須の条件を、系統に当てる。
pub fn check_all<F: CryptoFamily>() -> Vec<Check> {
    let checks: [(&'static str, CheckFn); 11] = [
        ("X-WingのHPKEで封じて開ける", hpke_round_trip::<F>),
        (
            "Ed25519・AEAD・HKDFを同じ系統で使える",
            primitives_round_trip::<F>,
        ),
        ("RFC 9180のHPKEの試験ベクトル", || {
            hpke_vector::<F>(HpkeKem::DhKemX25519, "rfc9180")
        }),
        (
            "X-WingのHPKEの試験ベクトル (draft-ietf-hpke-pq)",
            || hpke_vector::<F>(HpkeKem::XWing, "draft-ietf-hpke-pq"),
        ),
        ("RFC 8032の署名の試験ベクトル", ed25519_rfc8032::<F>),
        ("Wycheproofの署名の試験ベクトル", ed25519_wycheproof::<F>),
        ("WycheproofのAEADの試験ベクトル", aead_wycheproof::<F>),
        ("WycheproofのHKDFの試験ベクトル", hkdf_wycheproof::<F>),
        ("HPKEの書き換えを拒む", hpke_tamper::<F>),
        ("署名の書き換えを拒む", signature_tamper::<F>),
        ("AEADの書き換えを拒む", aead_tamper::<F>),
    ];
    checks
        .into_iter()
        .map(|(name, check)| Check {
            name,
            result: check(),
        })
        .collect()
}

/// ed25519-speccheckの12の例を、系統の確かめの入口で受け付けるか。
pub fn speccheck<F: CryptoFamily>() -> Vec<bool> {
    list(&ED25519_VECTORS, "speccheck")
        .iter()
        .map(|case| {
            F::verify(
                &bytes(case, "pub_key"),
                &bytes(case, "message"),
                &bytes(case, "signature"),
            )
            .is_ok()
        })
        .collect()
}

/// Wycheproofの署名の試験ベクトルのうち、`acceptable` (受け付けても拒んでもよい) の件数と、受け付けた件数。
pub fn acceptable_signatures<F: CryptoFamily>() -> (usize, usize) {
    let acceptable: Vec<_> = list(&ED25519_VECTORS, "wycheproof")
        .iter()
        .filter(|vector| text(vector, "result") == "acceptable")
        .collect();
    let accepted = acceptable
        .iter()
        .filter(|vector| {
            F::verify(
                &bytes(vector, "pk"),
                &bytes(vector, "msg"),
                &bytes(vector, "sig"),
            )
            .is_ok()
        })
        .count();
    (acceptable.len(), accepted)
}

fn hex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|i| {
            text.get(i..i + 2)
                .and_then(|pair| u8::from_str_radix(pair, 16).ok())
                .expect("同梱した試験ベクトルの値は16進の文字列")
        })
        .collect()
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key]
        .as_str()
        .expect("同梱した試験ベクトルの値は文字列")
}

fn bytes(value: &Value, key: &str) -> Vec<u8> {
    hex(text(value, key))
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key]
        .as_array()
        .expect("同梱した試験ベクトルの一覧は配列")
}

/// 条件を満たさないときに、説明を付けて失敗にする。
fn ensure(ok: bool, message: impl FnOnce() -> String) -> Result<(), String> {
    if ok { Ok(()) } else { Err(message()) }
}

/// バイト列の各バイトの1ビットを順に反転した列を返す。反転するビットはバイトの位置で変える。
fn bit_flips(bytes: &[u8]) -> impl Iterator<Item = Vec<u8>> + '_ {
    (0..bytes.len()).map(|i| {
        let mut flipped = bytes.to_vec();
        flipped[i] ^= 1 << (i % 8);
        flipped
    })
}

const INFO: &[u8] = b"crypto-comparison info";
const AAD: &[u8] = b"crypto-comparison aad";
const PLAINTEXT: &[u8] = b"role keys: topic key and two signing keys";

fn hpke_round_trip<F: CryptoFamily>() -> Result<(), String> {
    let keypair = F::generate_wrap_keypair(HpkeKem::XWing).map_err(|err| err.to_string())?;
    let sealed = F::seal(HpkeKem::XWing, &keypair.public_key, INFO, AAD, PLAINTEXT)
        .map_err(|err| err.to_string())?;
    let opened = F::open(
        HpkeKem::XWing,
        &keypair.secret_key,
        &sealed.enc,
        INFO,
        AAD,
        &sealed.ciphertext,
    )
    .map_err(|err| err.to_string())?;
    ensure(opened.as_bytes() == PLAINTEXT, || {
        "開いた平文が封じた平文と違う".to_owned()
    })
}

fn primitives_round_trip<F: CryptoFamily>() -> Result<(), String> {
    let signing = F::generate_signing_keypair().map_err(|err| err.to_string())?;
    let signature = F::sign(&signing.secret_key, PLAINTEXT).map_err(|err| err.to_string())?;
    F::verify(&signing.public_key, PLAINTEXT, &signature).map_err(|err| err.to_string())?;

    for algorithm in AeadAlgorithm::ALL {
        let key = crate::random_secret(AeadAlgorithm::KEY_LEN).map_err(|err| err.to_string())?;
        let nonce = crate::random_secret(algorithm.nonce_len()).map_err(|err| err.to_string())?;
        let ciphertext = F::aead_encrypt(algorithm, &key, nonce.as_bytes(), AAD, PLAINTEXT)
            .map_err(|err| format!("{}: {err}", algorithm.name()))?;
        let plaintext = F::aead_decrypt(algorithm, &key, nonce.as_bytes(), AAD, &ciphertext)
            .map_err(|err| format!("{}: {err}", algorithm.name()))?;
        ensure(plaintext.as_bytes() == PLAINTEXT, || {
            format!("{}: 復号した平文が違う", algorithm.name())
        })?;
    }

    let ikm = crate::random_secret(32).map_err(|err| err.to_string())?;
    let first = F::hkdf_sha256(b"salt", &ikm, b"info", 32).map_err(|err| err.to_string())?;
    let second = F::hkdf_sha256(b"salt", &ikm, b"info", 32).map_err(|err| err.to_string())?;
    let other = F::hkdf_sha256(b"salt", &ikm, b"other", 32).map_err(|err| err.to_string())?;
    ensure(first == second && first != other, || {
        "HKDFが同じ入力で同じ鍵を、別のinfoで別の鍵を導かない".to_owned()
    })
}

fn hpke_vector<F: CryptoFamily>(kem: HpkeKem, name: &str) -> Result<(), String> {
    let vector = &parse(HPKE)[name];
    let keypair = F::derive_wrap_keypair(kem, &bytes(vector, "ikmR"))
        .map_err(|err| format!("受け手の鍵を導けなかった: {err}"))?;
    // 秘密鍵のバイト列は比べない。RustCryptoのX25519は、クランプした形で書き出すため。
    ensure(keypair.public_key == bytes(vector, "pkRm"), || {
        "導いた受け手の公開鍵が違う".to_owned()
    })?;
    let encryption = &vector["encryption"];
    let plaintext = F::open(
        kem,
        &keypair.secret_key,
        &bytes(vector, "enc"),
        &bytes(vector, "info"),
        &bytes(encryption, "aad"),
        &bytes(encryption, "ct"),
    )
    .map_err(|err| format!("暗号文を開けなかった: {err}"))?;
    ensure(plaintext.as_bytes() == bytes(encryption, "pt"), || {
        "開いた平文が違う".to_owned()
    })
}

fn ed25519_rfc8032<F: CryptoFamily>() -> Result<(), String> {
    for vector in list(&ED25519_VECTORS, "rfc8032") {
        let name = text(vector, "name");
        let seed = SecretBytes::new(bytes(vector, "secret_key"));
        let keypair =
            F::signing_keypair_from_seed(&seed).map_err(|err| format!("{name}: {err}"))?;
        ensure(keypair.public_key == bytes(vector, "public_key"), || {
            format!("{name}: 公開鍵が違う")
        })?;
        let message = bytes(vector, "message");
        let signature = F::sign(&seed, &message).map_err(|err| format!("{name}: {err}"))?;
        ensure(signature == bytes(vector, "signature"), || {
            format!("{name}: 署名が違う")
        })?;
        F::verify(&keypair.public_key, &message, &signature)
            .map_err(|err| format!("{name}: {err}"))?;
    }
    Ok(())
}

/// Wycheproofの `valid` を受け付け、`invalid` を拒むことを確かめる。`acceptable` は確かめない。
fn ed25519_wycheproof<F: CryptoFamily>() -> Result<(), String> {
    for vector in list(&ED25519_VECTORS, "wycheproof") {
        let id = &vector["tcId"];
        let verified = F::verify(
            &bytes(vector, "pk"),
            &bytes(vector, "msg"),
            &bytes(vector, "sig"),
        );
        match text(vector, "result") {
            "valid" => ensure(verified.is_ok(), || {
                format!("正しい署名を拒んだ (tcId {id})")
            })?,
            "invalid" => ensure(verified.is_err(), || {
                format!("正しくない署名を受け付けた (tcId {id})")
            })?,
            _ => {}
        }
    }
    Ok(())
}

fn aead_wycheproof<F: CryptoFamily>() -> Result<(), String> {
    let all = parse(AEAD);
    for algorithm in AeadAlgorithm::ALL {
        let group = match algorithm {
            AeadAlgorithm::Aes256Gcm => "aes-256-gcm",
            AeadAlgorithm::ChaCha20Poly1305 => "chacha20-poly1305",
            AeadAlgorithm::XChaCha20Poly1305 => "xchacha20-poly1305",
        };
        let name = algorithm.name();
        for vector in list(&all, group) {
            let id = &vector["tcId"];
            let key = SecretBytes::new(bytes(vector, "key"));
            let nonce = bytes(vector, "iv");
            let aad = bytes(vector, "aad");
            let msg = bytes(vector, "msg");
            let mut sealed = bytes(vector, "ct");
            sealed.extend(bytes(vector, "tag"));
            let opened = F::aead_decrypt(algorithm, &key, &nonce, &aad, &sealed);
            if text(vector, "result") == "valid" {
                let encrypted = F::aead_encrypt(algorithm, &key, &nonce, &aad, &msg)
                    .map_err(|err| format!("{name} (tcId {id}): {err}"))?;
                ensure(encrypted == sealed, || {
                    format!("{name}: 暗号文が違う (tcId {id})")
                })?;
                let opened = opened.map_err(|err| format!("{name} (tcId {id}): {err}"))?;
                ensure(opened.as_bytes() == msg, || {
                    format!("{name}: 復号した平文が違う (tcId {id})")
                })?;
            } else {
                ensure(opened.is_err(), || {
                    format!("{name}: 正しくない暗号文を復号した (tcId {id})")
                })?;
            }
        }
    }
    Ok(())
}

fn hkdf_wycheproof<F: CryptoFamily>() -> Result<(), String> {
    for vector in list(&parse(HKDF), "vectors") {
        let id = &vector["tcId"];
        let size = vector["size"]
            .as_u64()
            .and_then(|size| usize::try_from(size).ok())
            .expect("同梱したHKDFの試験ベクトルの長さは整数");
        let okm = F::hkdf_sha256(
            &bytes(vector, "salt"),
            &SecretBytes::new(bytes(vector, "ikm")),
            &bytes(vector, "info"),
            size,
        );
        if text(vector, "result") == "valid" {
            let okm = okm.map_err(|err| format!("tcId {id}: {err}"))?;
            ensure(okm.as_bytes() == bytes(vector, "okm"), || {
                format!("導いた鍵が違う (tcId {id})")
            })?;
        } else {
            ensure(okm.is_err(), || {
                format!("導けない長さの鍵を導いた (tcId {id})")
            })?;
        }
    }
    Ok(())
}

fn hpke_tamper<F: CryptoFamily>() -> Result<(), String> {
    let kem = HpkeKem::XWing;
    let keypair = F::generate_wrap_keypair(kem).map_err(|err| err.to_string())?;
    let sealed =
        F::seal(kem, &keypair.public_key, INFO, AAD, PLAINTEXT).map_err(|err| err.to_string())?;
    let open = |enc: &[u8], info: &[u8], aad: &[u8], ciphertext: &[u8]| {
        F::open(kem, &keypair.secret_key, enc, info, aad, ciphertext).is_ok()
    };
    for (what, accepted) in [
        (
            "カプセル",
            bit_flips(&sealed.enc).any(|enc| open(&enc, INFO, AAD, &sealed.ciphertext)),
        ),
        (
            "info",
            bit_flips(INFO).any(|info| open(&sealed.enc, &info, AAD, &sealed.ciphertext)),
        ),
        (
            "追加データ",
            bit_flips(AAD).any(|aad| open(&sealed.enc, INFO, &aad, &sealed.ciphertext)),
        ),
        (
            "暗号文",
            bit_flips(&sealed.ciphertext)
                .any(|ciphertext| open(&sealed.enc, INFO, AAD, &ciphertext)),
        ),
    ] {
        ensure(!accepted, || format!("{what}を書き換えた暗号文を開いた"))?;
    }
    let other = F::generate_wrap_keypair(kem).map_err(|err| err.to_string())?;
    ensure(
        F::open(
            kem,
            &other.secret_key,
            &sealed.enc,
            INFO,
            AAD,
            &sealed.ciphertext,
        )
        .is_err(),
        || "別の秘密鍵で暗号文を開いた".to_owned(),
    )
}

fn signature_tamper<F: CryptoFamily>() -> Result<(), String> {
    let keypair = F::generate_signing_keypair().map_err(|err| err.to_string())?;
    let signature = F::sign(&keypair.secret_key, PLAINTEXT).map_err(|err| err.to_string())?;
    let verify = |public_key: &[u8], message: &[u8], signature: &[u8]| {
        F::verify(public_key, message, signature).is_ok()
    };
    for (what, accepted) in [
        (
            "署名",
            bit_flips(&signature)
                .any(|signature| verify(&keypair.public_key, PLAINTEXT, &signature)),
        ),
        (
            "署名の対象",
            bit_flips(PLAINTEXT).any(|message| verify(&keypair.public_key, &message, &signature)),
        ),
        (
            "公開鍵",
            bit_flips(&keypair.public_key)
                .any(|public_key| verify(&public_key, PLAINTEXT, &signature)),
        ),
    ] {
        ensure(!accepted, || format!("{what}を書き換えた署名を受け付けた"))?;
    }
    Ok(())
}

fn aead_tamper<F: CryptoFamily>() -> Result<(), String> {
    for algorithm in AeadAlgorithm::ALL {
        let key = crate::random_secret(AeadAlgorithm::KEY_LEN).map_err(|err| err.to_string())?;
        let nonce = crate::random_secret(algorithm.nonce_len()).map_err(|err| err.to_string())?;
        let nonce = nonce.as_bytes();
        let ciphertext = F::aead_encrypt(algorithm, &key, nonce, AAD, PLAINTEXT)
            .map_err(|err| format!("{}: {err}", algorithm.name()))?;
        let decrypt = |nonce: &[u8], aad: &[u8], ciphertext: &[u8]| {
            F::aead_decrypt(algorithm, &key, nonce, aad, ciphertext).is_ok()
        };
        for (what, accepted) in [
            (
                "nonce",
                bit_flips(nonce).any(|nonce| decrypt(&nonce, AAD, &ciphertext)),
            ),
            (
                "追加データ",
                bit_flips(AAD).any(|aad| decrypt(nonce, &aad, &ciphertext)),
            ),
            (
                "暗号文とタグ",
                bit_flips(&ciphertext).any(|ciphertext| decrypt(nonce, AAD, &ciphertext)),
            ),
        ] {
            ensure(!accepted, || {
                format!("{}: {what}を書き換えた暗号文を復号した", algorithm.name())
            })?;
        }
    }
    Ok(())
}
