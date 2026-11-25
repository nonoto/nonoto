//! 暗号の部品の系統 (RustCrypto・libcrux) に、同じ操作を当てて比べる。
//!
//! 使い方: `cargo run --release -p crypto-comparison -- [--runs 回数]`
//! 結果はMarkdownで標準出力に書く。最初に系統ごとに必須の条件を満たしたかを、続けて計測の値を表にする。

use std::fmt::Write as _;
use std::num::NonZeroUsize;
use std::process::ExitCode;
use std::time::Duration;

use crypto_comparison::conformance::{self, Check};
use crypto_comparison::measure::{
    self, AEAD_PLAINTEXT_LEN, DEFAULT_RUNS, Measurement, Report, SIGN_MESSAGE_LEN, Samples, Timing,
    WRAP_PLAINTEXT_LEN,
};
use crypto_comparison::{AeadAlgorithm, CryptoError, CryptoFamily};

/// 1つの系統の結果。
struct Family {
    name: &'static str,
    verify_entry: &'static str,
    checks: Vec<Check>,
    report: Report,
    speccheck: Vec<bool>,
    acceptable: (usize, usize),
}

fn family<F: CryptoFamily>(samples: &mut Samples) -> Family {
    eprintln!("{}を確かめて計測している", F::NAME);
    Family {
        name: F::NAME,
        verify_entry: F::VERIFY_ENTRY,
        checks: conformance::check_all::<F>(),
        report: measure::run::<F>(samples),
        speccheck: conformance::speccheck::<F>(),
        acceptable: conformance::acceptable_signatures::<F>(),
    }
}

fn us(duration: Duration) -> String {
    format!("{:.1}µs", duration.as_secs_f64() * 1_000_000.0)
}

fn timing(timing: Timing) -> String {
    format!(
        "{} / {} / {}",
        us(timing.median),
        us(timing.p99),
        us(timing.max)
    )
}

fn mark(ok: bool) -> &'static str {
    if ok {
        "満たした"
    } else {
        "満たさない"
    }
}

fn yes(ok: bool) -> &'static str {
    if ok { "できる" } else { "できない" }
}

/// 系統ごとに1列の表の行を書く。
fn row(out: &mut String, families: &[Family], label: &str, cell: impl Fn(&Family) -> String) {
    let cells: Vec<String> = families.iter().map(cell).collect();
    let _ = writeln!(out, "| {label} | {} |", cells.join(" | "));
}

fn header(out: &mut String, families: &[Family], first: &str) {
    let names: Vec<&str> = families.iter().map(|family| family.name).collect();
    let _ = writeln!(out, "| {first} | {} |", names.join(" | "));
    let _ = writeln!(out, "| --- |{}", " --- |".repeat(names.len()));
}

type InteropResult = (&'static str, measure::Interop);

fn render(runs: NonZeroUsize, families: &[Family], interop: &[InteropResult]) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# 暗号の部品の計測");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "- 時間: 操作ごとに{runs}回計り、中央値 / p99 / 最大を示す"
    );
    let _ = writeln!(
        out,
        "- HPKEはX-Wing・HKDF-SHA256・ChaCha20-Poly1305のbase modeで、{WRAP_PLAINTEXT_LEN}バイトを封じる"
    );
    let _ = writeln!(
        out,
        "- 署名の対象は{SIGN_MESSAGE_LEN}バイト、AEADの平文は{AEAD_PLAINTEXT_LEN}バイト、HKDFは32バイトを導く"
    );
    let _ = writeln!(out);
    render_checks(&mut out, families);
    render_measurements(
        &mut out,
        families,
        "時間 (中央値 / p99 / 最大)",
        "操作",
        |report| &report.timings,
        timing,
    );
    render_measurements(
        &mut out,
        families,
        "大きさ",
        "値",
        |report| &report.sizes,
        |size| format!("{size}B"),
    );
    render_signatures(&mut out, families);
    render_interop(&mut out, interop);
    render_nonces(&mut out);
    out
}

fn render_checks(out: &mut String, families: &[Family]) {
    let _ = writeln!(out, "## 必須の条件");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "この表は実行時の検査の判定を示す。各OS向けのビルドはCIで確かめる。"
    );
    let _ = writeln!(out);
    header(out, families, "条件");
    for (i, check) in families[0].checks.iter().enumerate() {
        row(out, families, check.name, |family| {
            mark(family.checks[i].result.is_ok()).to_owned()
        });
    }
    row(out, families, "判定", |family| {
        mark(family.checks.iter().all(|check| check.result.is_ok())).to_owned()
    });
    let _ = writeln!(out);
    let failures: Vec<String> = families
        .iter()
        .flat_map(|family| {
            family.checks.iter().filter_map(|check| {
                check
                    .result
                    .as_ref()
                    .err()
                    .map(|reason| format!("- {}: {}: {reason}", family.name, check.name))
            })
        })
        .collect();
    if !failures.is_empty() {
        let _ = writeln!(out, "{}", failures.join("\n"));
        let _ = writeln!(out);
    }
}

fn render_measurements<T: Copy>(
    out: &mut String,
    families: &[Family],
    title: &str,
    first_column: &str,
    select: impl Fn(&Report) -> &[Measurement<T>],
    format_value: impl Fn(T) -> String,
) {
    let _ = writeln!(out, "## {title}");
    let _ = writeln!(out);
    header(out, families, first_column);
    for (index, (name, _)) in select(&families[0].report).iter().enumerate() {
        row(out, families, name, |family| {
            match select(&family.report)[index].1 {
                Ok(value) => format_value(value),
                Err(err) => format!("計測できない ({err})"),
            }
        });
    }
    let _ = writeln!(out);
}

fn render_signatures(out: &mut String, families: &[Family]) {
    let _ = writeln!(out, "## Ed25519の確かめで受け付ける署名");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "ed25519-speccheckの例0から11を受け付けたか (Vは受け付けた、Xは拒んだ)。"
    );
    let _ = writeln!(out);
    let numbers: Vec<String> = (0..families[0].speccheck.len())
        .map(|i| i.to_string())
        .collect();
    let _ = writeln!(
        out,
        "| 系統 | 入口 | {} | Wycheproofのacceptable |",
        numbers.join(" | ")
    );
    let _ = writeln!(out, "| --- | --- |{} --- |", " --- |".repeat(numbers.len()));
    for family in families {
        let cells: Vec<&str> = family
            .speccheck
            .iter()
            .map(|&ok| if ok { "V" } else { "X" })
            .collect();
        let (total, accepted) = family.acceptable;
        let _ = writeln!(
            out,
            "| {} | {} | {} | {accepted}/{total}件を受け付けた |",
            family.name,
            family.verify_entry,
            cells.join(" | ")
        );
    }
    let _ = writeln!(out);
}

fn render_interop(out: &mut String, interop: &[InteropResult]) {
    if interop.is_empty() {
        return;
    }

    let _ = writeln!(out, "## 系統の間の受け渡し");
    let names: Vec<&str> = interop[0].1.items().iter().map(|(name, _)| *name).collect();
    let _ = writeln!(out, "| 向き | {} |", names.join(" | "));
    let _ = writeln!(out, "| --- |{}", " --- |".repeat(names.len()));
    for (direction, result) in interop {
        let cells: Vec<&str> = result
            .items()
            .iter()
            .map(|(_, item)| match item {
                Ok(ok) => yes(*ok),
                Err(_) => "—",
            })
            .collect();
        let _ = writeln!(out, "| {direction} | {} |", cells.join(" | "));
    }
    let _ = writeln!(out);

    for (direction, result) in interop {
        for (name, item) in result.items() {
            if let Err(err) = item {
                let _ = writeln!(out, "- {direction}の{name}を確かめられなかった ({err})");
            }
        }
    }
    let _ = writeln!(out);
}

fn render_nonces(out: &mut String) {
    let _ = writeln!(out, "## AEADのnonce");
    let _ = writeln!(out);
    let _ = writeln!(
        out,
        "| 方式 | nonceの長さ | 乱数のnonceで1つの鍵に使える回数の目安 (重なる確率を2^-32以下に抑える) |"
    );
    let _ = writeln!(out, "| --- | --- | --- |");
    for algorithm in AeadAlgorithm::ALL {
        let _ = writeln!(
            out,
            "| {} | {}バイト | 2^{}回 |",
            algorithm.name(),
            algorithm.nonce_len(),
            algorithm.random_nonce_limit_log2()
        );
    }
}

fn comparison(
    runs: NonZeroUsize,
    families: &[Family],
    interop: &[InteropResult],
) -> (String, ExitCode) {
    let failed = families.iter().any(|family| family.report.has_errors())
        || interop.iter().any(|(_, result)| result.has_errors());
    let status = if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    };
    (render(runs, families, interop), status)
}

fn run(runs: NonZeroUsize) -> Result<(String, ExitCode), CryptoError> {
    #[cfg(all(feature = "rustcrypto", feature = "libcrux"))]
    use crypto_comparison::{Libcrux, RustCrypto};

    let mut samples = Samples::try_new(runs)?;
    let families = [
        #[cfg(feature = "rustcrypto")]
        family::<crypto_comparison::RustCrypto>(&mut samples),
        #[cfg(feature = "libcrux")]
        family::<crypto_comparison::Libcrux>(&mut samples),
    ];
    let interop: &[InteropResult] = &[
        #[cfg(all(feature = "rustcrypto", feature = "libcrux"))]
        (
            "RustCrypto → libcrux",
            measure::interop::<RustCrypto, Libcrux>(),
        ),
        #[cfg(all(feature = "rustcrypto", feature = "libcrux"))]
        (
            "libcrux → RustCrypto",
            measure::interop::<Libcrux, RustCrypto>(),
        ),
    ];

    Ok(comparison(runs, &families, interop))
}

/// 引数から、操作ごとに計る回数を読む。
fn parse_runs() -> Result<NonZeroUsize, String> {
    let mut runs = NonZeroUsize::new(DEFAULT_RUNS).expect("既定の計測回数は1以上");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--runs" => {
                runs = args
                    .next()
                    .and_then(|value| value.parse().ok())
                    .ok_or("--runs は1以上の整数で指定する")?;
            }
            _ => return Err(format!("知らない引数: {arg}")),
        }
    }
    Ok(runs)
}

fn main() -> ExitCode {
    let runs = match parse_runs() {
        Ok(runs) => runs,
        Err(err) => {
            eprintln!("エラー: {err}");
            return ExitCode::FAILURE;
        }
    };
    match run(runs) {
        Ok((out, status)) => {
            print!("{out}");
            status
        }
        Err(err) => {
            eprintln!("エラー: {err}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(all(test, feature = "rustcrypto"))]
mod tests {
    use super::*;
    use crypto_comparison::{
        HpkeKem, RustCrypto, Sealed, SecretBytes, SigningKeyPair, WrapKeyPair,
    };

    struct HpkeUnavailable;

    impl CryptoFamily for HpkeUnavailable {
        const NAME: &'static str = "HPKEが動かない系統";
        const VERIFY_ENTRY: &'static str = RustCrypto::VERIFY_ENTRY;

        fn generate_wrap_keypair(_: HpkeKem) -> Result<WrapKeyPair, CryptoError> {
            Err(CryptoError::Random)
        }

        fn derive_wrap_keypair(_: HpkeKem, _: &[u8]) -> Result<WrapKeyPair, CryptoError> {
            Err(CryptoError::Random)
        }

        fn seal(_: HpkeKem, _: &[u8], _: &[u8], _: &[u8], _: &[u8]) -> Result<Sealed, CryptoError> {
            Err(CryptoError::Seal)
        }

        fn open(
            _: HpkeKem,
            _: &SecretBytes,
            _: &[u8],
            _: &[u8],
            _: &[u8],
            _: &[u8],
        ) -> Result<SecretBytes, CryptoError> {
            Err(CryptoError::Open)
        }

        fn signing_keypair_from_seed(seed: &SecretBytes) -> Result<SigningKeyPair, CryptoError> {
            RustCrypto::signing_keypair_from_seed(seed)
        }

        fn sign(secret_key: &SecretBytes, message: &[u8]) -> Result<Vec<u8>, CryptoError> {
            RustCrypto::sign(secret_key, message)
        }

        fn verify(public_key: &[u8], message: &[u8], signature: &[u8]) -> Result<(), CryptoError> {
            RustCrypto::verify(public_key, message, signature)
        }

        fn aead_encrypt(
            algorithm: AeadAlgorithm,
            key: &SecretBytes,
            nonce: &[u8],
            aad: &[u8],
            plaintext: &[u8],
        ) -> Result<Vec<u8>, CryptoError> {
            RustCrypto::aead_encrypt(algorithm, key, nonce, aad, plaintext)
        }

        fn aead_decrypt(
            algorithm: AeadAlgorithm,
            key: &SecretBytes,
            nonce: &[u8],
            aad: &[u8],
            ciphertext: &[u8],
        ) -> Result<SecretBytes, CryptoError> {
            RustCrypto::aead_decrypt(algorithm, key, nonce, aad, ciphertext)
        }

        fn hkdf_sha256(
            salt: &[u8],
            ikm: &SecretBytes,
            info: &[u8],
            len: usize,
        ) -> Result<SecretBytes, CryptoError> {
            RustCrypto::hkdf_sha256(salt, ikm, info, len)
        }
    }

    #[test]
    fn 片方のhpkeが失敗しても両系統の判定と取得できた計測値を表示する() {
        let runs = NonZeroUsize::MIN;
        let mut samples = Samples::try_new(runs).expect("標本のバッファを確保できなかった");
        let families = [
            family::<HpkeUnavailable>(&mut samples),
            family::<RustCrypto>(&mut samples),
        ];
        let interop = [(
            "HPKEが動かない系統 → RustCrypto",
            measure::interop::<HpkeUnavailable, RustCrypto>(),
        )];
        let (output, status) = comparison(runs, &families, &interop);

        assert_eq!(
            status,
            ExitCode::FAILURE,
            "計測の失敗を終了コードに反映していない"
        );
        assert!(
            output.contains("| 判定 | 満たさない | 満たした |"),
            "両系統の適合性を表示していない"
        );
        assert!(
            output
                .contains("HPKEが動かない系統: X-WingのHPKEで封じて開ける: 乱数を取得できなかった"),
            "必須条件を満たさなかった理由を表示していない"
        );
        let seal = output
            .lines()
            .find(|line| line.starts_with("| HPKEで封じる |"))
            .expect("HPKEの計測結果が無い");
        assert!(
            seal.contains("計測できない (乱数を取得できなかった)"),
            "失敗の理由が無い"
        );
        assert!(seal.contains("µs"), "もう片方のHPKEの計測値が無い");
        let sign = output
            .lines()
            .find(|line| line.starts_with("| Ed25519で署名する |"))
            .expect("Ed25519の計測結果が無い");
        assert_eq!(
            sign.matches("µs").count(),
            6,
            "両方の系統の署名の計測値が揃っていない"
        );
        assert!(
            output
                .contains("| HPKEが動かない系統 → RustCrypto | — | — | できる | できる | できる |"),
            "HPKEの失敗で、ほかの部品の受け渡しの結果が消えた"
        );
        assert!(
            output.contains(
                "HPKEが動かない系統 → RustCryptoのX-Wingの鍵の導出を確かめられなかった (乱数を取得できなかった)"
            ),
            "鍵の導出を確かめられなかった理由が無い"
        );
        assert!(
            output.contains(
                "HPKEが動かない系統 → RustCryptoのHPKEを確かめられなかった (HPKEで封じられなかった)"
            ),
            "HPKEを確かめられなかった理由が無い"
        );
    }
}
