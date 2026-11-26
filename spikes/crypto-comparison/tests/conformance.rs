//! 必須の条件と、系統ごとに記録する振る舞いを確かめる。

use crypto_comparison::conformance;
use crypto_comparison::measure;
use crypto_comparison::{CryptoError, CryptoFamily, Libcrux, RustCrypto, SecretBytes};

fn assert_all_checks_pass<F: CryptoFamily>() {
    for check in conformance::check_all::<F>() {
        if let Err(reason) = check.result {
            panic!("{}: 「{}」を満たさない: {reason}", F::NAME, check.name);
        }
    }
}

#[test]
fn rustcryptoが必須の条件をすべて満たす() {
    assert_all_checks_pass::<RustCrypto>();
}

#[test]
fn libcruxが必須の条件をすべて満たす() {
    assert_all_checks_pass::<Libcrux>();
}

/// ed25519-speccheckの例4から11 (Sが範囲の外・Rや公開鍵が正規でない符号化など) は、どちらの系統も拒む。
fn assert_rejects_malleable_signatures<F: CryptoFamily>() {
    let accepted = conformance::speccheck::<F>();
    assert_eq!(accepted.len(), 12, "ed25519-speccheckの例が12件ない");
    for (i, accepted) in accepted.iter().enumerate().skip(4) {
        assert!(
            !accepted,
            "{}: ed25519-speccheckの例{i}を受け付けた",
            F::NAME
        );
    }
}

#[test]
fn rustcryptoが範囲の外のsや正規でない符号化の署名を拒む() {
    assert_rejects_malleable_signatures::<RustCrypto>();
}

#[test]
fn libcruxが範囲の外のsや正規でない符号化の署名を拒む() {
    assert_rejects_malleable_signatures::<Libcrux>();
}

#[test]
fn rustcryptoは位数の小さい公開鍵とrの署名を拒み_libcruxは受け付ける() {
    for i in 0..3 {
        assert!(
            !conformance::speccheck::<RustCrypto>()[i],
            "RustCryptoがed25519-speccheckの例{i}を受け付けた"
        );
        assert!(
            conformance::speccheck::<Libcrux>()[i],
            "libcruxがed25519-speccheckの例{i}を拒んだ"
        );
    }
}

#[test]
fn rustcryptoが封じたものや署名をlibcruxで扱える() {
    let interop = measure::interop::<RustCrypto, Libcrux>();
    assert!(
        interop.all(),
        "RustCryptoからlibcruxへ受け渡せない: {interop:?}"
    );
}

#[test]
fn libcruxが封じたものや署名をrustcryptoで扱える() {
    let interop = measure::interop::<Libcrux, RustCrypto>();
    assert!(
        interop.all(),
        "libcruxからRustCryptoへ受け渡せない: {interop:?}"
    );
}

fn assert_hkdf_output_limit<F: CryptoFamily>() {
    let ikm = SecretBytes::new(vec![42; 32]);
    let longest =
        F::hkdf_sha256(b"salt", &ikm, b"info", 8160).expect("上限の長さの鍵を導けなかった");
    assert_eq!(longest.len(), 8160, "{}: 出力長が違う", F::NAME);

    for len in [8161, usize::MAX] {
        assert_eq!(
            F::hkdf_sha256(b"salt", &ikm, b"info", len),
            Err(CryptoError::Derive),
            "{}: HKDFの上限を超える長さ {len} を拒まなかった",
            F::NAME
        );
    }
}

#[test]
fn どちらの系統もメモリ確保より先にhkdfの出力上限を検査する() {
    assert_hkdf_output_limit::<RustCrypto>();
    assert_hkdf_output_limit::<Libcrux>();
}
