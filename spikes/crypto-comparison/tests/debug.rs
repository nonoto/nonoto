//! デバッグ表示が秘密鍵を出さないことを確かめる。

use std::fmt::Write as _;

use crypto_comparison::{CryptoFamily, HpkeKem, Libcrux, RustCrypto, SecretBytes};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, byte| {
        let _ = write!(out, "{byte:02x}");
        out
    })
}

fn assert_secret_hidden<F: CryptoFamily>() {
    let wrap = F::generate_wrap_keypair(HpkeKem::XWing).expect("包むための鍵の組を作れなかった");
    let signing = F::generate_signing_keypair().expect("署名の鍵の組を作れなかった");
    for (what, debug, secret) in [
        ("包むための鍵の組", format!("{wrap:?}"), &wrap.secret_key),
        ("署名の鍵の組", format!("{signing:?}"), &signing.secret_key),
    ] {
        assert!(
            !debug.contains(&format!("{:?}", secret.as_bytes()))
                && !debug.contains(&hex(secret.as_bytes())),
            "{}: {what}のデバッグ表示に秘密鍵が含まれている",
            F::NAME
        );
    }
}

#[test]
fn rustcryptoの鍵の組をデバッグ表示しても秘密鍵を出さない() {
    assert_secret_hidden::<RustCrypto>();
}

#[test]
fn libcruxの鍵の組をデバッグ表示しても秘密鍵を出さない() {
    assert_secret_hidden::<Libcrux>();
}

#[test]
fn 秘密の値のデバッグ表示は長さだけを出す() {
    let secret = SecretBytes::new(vec![0x42; 32]);
    assert_eq!(format!("{secret:?}"), "SecretBytes(32バイト)");
}
