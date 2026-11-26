//! 時間と大きさの計測と、系統の間で暗号文と署名を受け渡せるかの確かめ。

use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use crate::{AeadAlgorithm, CryptoError, CryptoFamily, HpkeKem, SecretBytes};

/// 1つの操作を計る、既定の回数。
pub const DEFAULT_RUNS: usize = 1000;

/// HPKEで包む平文の大きさ。役割の鍵 (トピックの鍵と、2つの書き込みの秘密鍵) を32バイトずつ包む想定。
pub const WRAP_PLAINTEXT_LEN: usize = 96;
/// 署名の対象の大きさ。
pub const SIGN_MESSAGE_LEN: usize = 256;
/// AEADで暗号化する平文の大きさ。
pub const AEAD_PLAINTEXT_LEN: usize = 1024;

const INFO: &[u8] = b"crypto-comparison measure info";
const AAD: &[u8] = b"crypto-comparison measure aad";

/// 時間の分布。
#[derive(Debug, Clone, Copy)]
pub struct Timing {
    pub median: Duration,
    pub p99: Duration,
    pub max: Duration,
}

impl Timing {
    fn from_samples(samples: &mut [Duration]) -> Self {
        samples.sort_unstable();
        let last = samples.len() - 1;
        // 先に100で割れる分を分け、百分位を求めるときのオーバーフローを避ける。
        let at =
            |percent: usize| samples[(last / 100) * percent + ((last % 100) * percent + 50) / 100];
        Self {
            median: at(50),
            p99: at(99),
            max: samples[last],
        }
    }
}

/// 全操作の計測で使い回す標本のバッファ。
#[derive(Debug)]
pub struct Samples {
    runs: NonZeroUsize,
    durations: Vec<Duration>,
}

impl Samples {
    pub fn try_new(runs: NonZeroUsize) -> Result<Self, CryptoError> {
        let mut durations = Vec::new();
        durations
            .try_reserve_exact(runs.get())
            .map_err(|_| CryptoError::SampleCapacity)?;
        Ok(Self { runs, durations })
    }

    fn time(
        &mut self,
        mut operation: impl FnMut() -> Result<(), CryptoError>,
    ) -> Result<Timing, CryptoError> {
        self.durations.clear();
        for _ in 0..self.runs.get() {
            let start = Instant::now();
            operation()?;
            self.durations.push(start.elapsed());
        }
        Ok(Timing::from_samples(&mut self.durations))
    }
}

/// 計測項目の名前と、値か計測ができなかった理由。
pub type Measurement<T> = (String, Result<T, CryptoError>);

/// 1つの系統の計測の結果。
#[derive(Debug, Default)]
pub struct Report {
    pub timings: Vec<Measurement<Timing>>,
    pub sizes: Vec<Measurement<usize>>,
}

impl Report {
    pub fn has_errors(&self) -> bool {
        self.timings.iter().any(|(_, value)| value.is_err())
            || self.sizes.iter().any(|(_, value)| value.is_err())
    }
}

fn measure_wrap<F: CryptoFamily>(samples: &mut Samples, report: &mut Report) {
    let kem = HpkeKem::XWing;
    let keygen = samples.time(|| F::generate_wrap_keypair(kem).map(drop));
    let keypair = F::generate_wrap_keypair(kem);
    let keypair = keypair.as_ref().map_err(|&err| err);
    let plaintext = vec![0x5a; WRAP_PLAINTEXT_LEN];

    let seal =
        |keypair: &crate::WrapKeyPair| F::seal(kem, &keypair.public_key, INFO, AAD, &plaintext);
    let seal_time = keypair.and_then(|keypair| samples.time(|| seal(keypair).map(drop)));
    let sealed = keypair.and_then(seal);
    let sealed = sealed.as_ref().map_err(|&err| err);
    let open_time = keypair.and_then(|keypair| {
        let sealed = sealed?;
        samples.time(|| {
            F::open(
                kem,
                &keypair.secret_key,
                &sealed.enc,
                INFO,
                AAD,
                &sealed.ciphertext,
            )
            .map(drop)
        })
    });

    report.timings.extend([
        ("X-Wingの鍵の組を作る".to_owned(), keygen),
        ("HPKEで封じる".to_owned(), seal_time),
        ("HPKEで開く".to_owned(), open_time),
    ]);
    report.sizes.extend([
        (
            "X-Wingの公開鍵".to_owned(),
            keypair.map(|key| key.public_key.len()),
        ),
        (
            "X-Wingの秘密鍵".to_owned(),
            keypair.map(|key| key.secret_key.len()),
        ),
        (
            "HPKEのカプセル".to_owned(),
            sealed.map(|value| value.enc.len()),
        ),
        (
            "HPKEで増える大きさ (カプセルを除く)".to_owned(),
            sealed.and_then(|value| {
                value
                    .ciphertext
                    .len()
                    .checked_sub(plaintext.len())
                    .ok_or(CryptoError::Seal)
            }),
        ),
    ]);
}

fn measure_signature<F: CryptoFamily>(samples: &mut Samples, report: &mut Report) {
    let keypair = F::generate_signing_keypair();
    let keypair = keypair.as_ref().map_err(|&err| err);
    let message = vec![0xa5; SIGN_MESSAGE_LEN];

    let sign = |keypair: &crate::SigningKeyPair| F::sign(&keypair.secret_key, &message);
    let sign_time = keypair.and_then(|keypair| samples.time(|| sign(keypair).map(drop)));
    let signature = keypair.and_then(sign);
    let signature = signature.as_ref().map_err(|&err| err);
    let verify_time = keypair.and_then(|keypair| {
        let signature = signature?;
        samples.time(|| F::verify(&keypair.public_key, &message, signature))
    });

    report.timings.extend([
        ("Ed25519で署名する".to_owned(), sign_time),
        ("Ed25519で確かめる".to_owned(), verify_time),
    ]);
    report.sizes.extend([
        (
            "Ed25519の公開鍵".to_owned(),
            keypair.map(|key| key.public_key.len()),
        ),
        ("Ed25519の署名".to_owned(), signature.map(Vec::len)),
    ]);
}

fn measure_aead<F: CryptoFamily>(
    samples: &mut Samples,
    report: &mut Report,
    algorithm: AeadAlgorithm,
) {
    let inputs = crate::random_secret(AeadAlgorithm::KEY_LEN)
        .and_then(|key| Ok((key, crate::random_secret(algorithm.nonce_len())?)));
    let inputs = inputs.as_ref().map_err(|&err| err);
    let plaintext = vec![0x3c; AEAD_PLAINTEXT_LEN];

    let encrypt = |(key, nonce): &(crate::SecretBytes, crate::SecretBytes)| {
        F::aead_encrypt(algorithm, key, nonce.as_bytes(), AAD, &plaintext)
    };
    let encrypt_time = inputs.and_then(|inputs| samples.time(|| encrypt(inputs).map(drop)));
    let ciphertext = inputs.and_then(encrypt);
    let ciphertext = ciphertext.as_ref().map_err(|&err| err);
    let decrypt_time = inputs.and_then(|(key, nonce)| {
        let ciphertext = ciphertext?;
        samples
            .time(|| F::aead_decrypt(algorithm, key, nonce.as_bytes(), AAD, ciphertext).map(drop))
    });

    let name = algorithm.name();
    report.timings.extend([
        (format!("{name}で暗号化する"), encrypt_time),
        (format!("{name}で復号する"), decrypt_time),
    ]);
    report.sizes.push((
        format!("{name}で増える大きさ (nonceを除く)"),
        ciphertext.and_then(|value| {
            value
                .len()
                .checked_sub(plaintext.len())
                .ok_or(CryptoError::Encrypt)
        }),
    ));
}

/// 1つの系統を計る。失敗した項目は理由を残し、残りの操作も計る。
pub fn run<F: CryptoFamily>(samples: &mut Samples) -> Report {
    let mut report = Report::default();
    measure_wrap::<F>(samples, &mut report);
    measure_signature::<F>(samples, &mut report);
    for algorithm in AeadAlgorithm::ALL {
        measure_aead::<F>(samples, &mut report, algorithm);
    }
    let hkdf = crate::random_secret(32)
        .and_then(|ikm| samples.time(|| F::hkdf_sha256(b"salt", &ikm, b"info", 32).map(drop)));
    report.timings.push(("HKDFで導く".to_owned(), hkdf));
    report
}

/// 系統 `A` の出力を系統 `B` で扱えるかの結果。
///
/// 項目ごとに、確かめた結果か、前提の操作が失敗して確かめられなかった理由を持つ。
/// 1つの項目の前提が失敗しても、ほかの項目は確かめる。
#[derive(Debug, Clone, Copy)]
pub struct Interop {
    /// 同じ `ikm` から、同じX-Wingの公開鍵を導くか。
    pub derive: Result<bool, CryptoError>,
    /// `B` の公開鍵に `A` が封じたものを、`B` が開けるか。
    pub hpke: Result<bool, CryptoError>,
    /// `A` の署名を `B` が確かめられるか。
    pub signature: Result<bool, CryptoError>,
    /// `A` が暗号化したものを `B` が復号できるか (すべてのAEADの方式で)。
    pub aead: Result<bool, CryptoError>,
    /// 同じ入力から、同じ鍵をHKDFで導くか。
    pub hkdf: Result<bool, CryptoError>,
}

impl Interop {
    /// 項目の名前と結果を、表の列の順に並べる。
    pub fn items(&self) -> [(&'static str, Result<bool, CryptoError>); 5] {
        [
            ("X-Wingの鍵の導出", self.derive),
            ("HPKE", self.hpke),
            ("Ed25519", self.signature),
            ("AEAD", self.aead),
            ("HKDF", self.hkdf),
        ]
    }

    /// すべての項目を確かめられ、受け渡せたか。
    pub fn all(&self) -> bool {
        self.items().iter().all(|(_, result)| *result == Ok(true))
    }

    /// 確かめられなかった項目があるか。
    pub fn has_errors(&self) -> bool {
        self.items().iter().any(|(_, result)| result.is_err())
    }
}

/// 系統 `A` の出力を、系統 `B` で扱えるかを確かめる。
pub fn interop<A: CryptoFamily, B: CryptoFamily>() -> Interop {
    let kem = HpkeKem::XWing;
    let plaintext = b"interop plaintext";
    let ikm = crate::random_secret(32);
    let ikm = ikm.as_ref().map_err(|&err| err);
    Interop {
        derive: ikm.and_then(|ikm| same_wrap_public_key::<A, B>(kem, ikm)),
        hpke: opens_sealed::<A, B>(kem, plaintext),
        signature: verifies_signature::<A, B>(plaintext),
        aead: decrypts_ciphertext::<A, B>(plaintext),
        hkdf: ikm.and_then(same_hkdf_output::<A, B>),
    }
}

fn same_wrap_public_key<A: CryptoFamily, B: CryptoFamily>(
    kem: HpkeKem,
    ikm: &SecretBytes,
) -> Result<bool, CryptoError> {
    Ok(A::derive_wrap_keypair(kem, ikm.as_bytes())?.public_key
        == B::derive_wrap_keypair(kem, ikm.as_bytes())?.public_key)
}

fn opens_sealed<A: CryptoFamily, B: CryptoFamily>(
    kem: HpkeKem,
    plaintext: &[u8],
) -> Result<bool, CryptoError> {
    let keypair = B::generate_wrap_keypair(kem)?;
    let sealed = A::seal(kem, &keypair.public_key, INFO, AAD, plaintext)?;
    Ok(B::open(
        kem,
        &keypair.secret_key,
        &sealed.enc,
        INFO,
        AAD,
        &sealed.ciphertext,
    )
    .is_ok_and(|opened| opened.as_bytes() == plaintext))
}

fn verifies_signature<A: CryptoFamily, B: CryptoFamily>(
    message: &[u8],
) -> Result<bool, CryptoError> {
    let signing = A::generate_signing_keypair()?;
    let signature = A::sign(&signing.secret_key, message)?;
    Ok(B::verify(&signing.public_key, message, &signature).is_ok()
        && B::signing_keypair_from_seed(&signing.secret_key)?.public_key == signing.public_key)
}

fn decrypts_ciphertext<A: CryptoFamily, B: CryptoFamily>(
    plaintext: &[u8],
) -> Result<bool, CryptoError> {
    let mut decrypts = true;
    for algorithm in AeadAlgorithm::ALL {
        let key = crate::random_secret(AeadAlgorithm::KEY_LEN)?;
        let nonce = crate::random_secret(algorithm.nonce_len())?;
        let ciphertext = A::aead_encrypt(algorithm, &key, nonce.as_bytes(), AAD, plaintext)?;
        decrypts &= B::aead_decrypt(algorithm, &key, nonce.as_bytes(), AAD, &ciphertext)
            .is_ok_and(|opened| opened.as_bytes() == plaintext);
    }
    Ok(decrypts)
}

fn same_hkdf_output<A: CryptoFamily, B: CryptoFamily>(
    ikm: &SecretBytes,
) -> Result<bool, CryptoError> {
    Ok(A::hkdf_sha256(b"salt", ikm, b"info", 64)? == B::hkdf_sha256(b"salt", ikm, b"info", 64)?)
}
