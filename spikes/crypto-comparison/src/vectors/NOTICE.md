# 同梱する試験ベクトル

Nonoto向けの比較で使う項目を、以下の固定版から抜き出して再構成した。
ベクトルの値は出典のまま使う。
比較バイナリを配布するときは、このファイルと下記のライセンス本文も添える。

## Wycheproofとed25519-speccheck

この節の試験ベクトルには [Apache License 2.0](./LICENSE-APACHE-2.0.txt) が適用される。

- `aead.json` はWycheproofの3ファイルから、256ビットの鍵、方式ごとの標準のnonce長、平文256バイト以下のケースを選んだ。
  判定と暗号化・復号に使うフィールドを、方式ごとの配列にまとめた。[AES-GCM](https://github.com/C2SP/wycheproof/blob/65ca494727a52944c204441c860b039e6786b99b/testvectors_v1/aes_gcm_test.json) [ChaCha20-Poly1305](https://github.com/C2SP/wycheproof/blob/65ca494727a52944c204441c860b039e6786b99b/testvectors_v1/chacha20_poly1305_test.json) [XChaCha20-Poly1305](https://github.com/C2SP/wycheproof/blob/65ca494727a52944c204441c860b039e6786b99b/testvectors_v1/xchacha20_poly1305_test.json)
- `ed25519.json` の `wycheproof` は、グループの公開鍵を各ケースに移し、署名の検査に使うフィールドを残した。[WycheproofのEd25519ベクトル](https://github.com/C2SP/wycheproof/blob/65ca494727a52944c204441c860b039e6786b99b/testvectors_v1/ed25519_test.json)
- `ed25519.json` の `speccheck` は、ed25519-speccheckの `cases.json` の12件を同じ順で取り込んだ。[ed25519-speccheckのベクトル](https://github.com/novifinancial/ed25519-speccheck/blob/65519336fda78a3d016e947df6d82848aca0c9da/cases.json)
- `hkdf.json` は各グループから、HKDF-SHA256の導出と判定に使うフィールドを1つの配列にまとめた。[WycheproofのHKDF-SHA256ベクトル](https://github.com/C2SP/wycheproof/blob/65ca494727a52944c204441c860b039e6786b99b/testvectors_v1/hkdf_sha256_test.json)

上流のライセンス本文は双方で同じ内容のため、1部を収録した。[WycheproofのLICENSE](https://github.com/C2SP/wycheproof/blob/65ca494727a52944c204441c860b039e6786b99b/LICENSE) [ed25519-speccheckのLICENSE](https://github.com/novifinancial/ed25519-speccheck/blob/65519336fda78a3d016e947df6d82848aca0c9da/LICENSE)

## IETFのRFCとInternet-Draft

この節の試験ベクトルには [IETF TrustのRevised BSD License](./LICENSE-IETF-BSD.txt) が適用される。[IETF Trust Legal Provisions 5.0 §4](https://trustee.ietf.org/documents/trust-legal-provisions/tlp-5/) [IETF TrustのCode Components一覧](https://trustee.ietf.org/documents/trust-legal-provisions/code-components-list-3/)

- `hpke.json` の `rfc9180` は、RFC 9180で参照しているJSONから、DHKEM(X25519, HKDF-SHA256)、HKDF-SHA256、ChaCha20-Poly1305のbase modeを選んだ。
  受け手の鍵の導出と最初の暗号文の開封に使うフィールドを残した。[RFC 9180 Appendix A](https://www.rfc-editor.org/rfc/rfc9180.html#appendix-A) [HPKEベクトル](https://github.com/cfrg/draft-irtf-cfrg-hpke/blob/5f503c564da00b0687b3de75f1dfbdfc4079ad31/test-vectors.json) [配布条件](https://github.com/cfrg/draft-irtf-cfrg-hpke/blob/5f503c564da00b0687b3de75f1dfbdfc4079ad31/CONTRIBUTING.md)
- `hpke.json` の `draft-ietf-hpke-pq` は、Internet-Draftの作業リポジトリのJSONから、X-Wing、HKDF-SHA256、ChaCha20-Poly1305のbase modeを選んだ。
  受け手の鍵の導出と最初の暗号文の開封に使うフィールドを残した。[HPKEのX-Wingベクトル](https://github.com/hpkewg/hpke-pq/blob/6433c8fce0b8b749dfc86c1095081a88698ccfab/test-vectors.json) [配布条件](https://github.com/hpkewg/hpke-pq/blob/6433c8fce0b8b749dfc86c1095081a88698ccfab/CONTRIBUTING.md)
- `ed25519.json` の `rfc8032` は、RFC 8032の7.1節から1024バイトのメッセージの例を除いた4件を選び、署名の検査に使うフィールドをJSONにした。[RFC 8032 §7.1](https://www.rfc-editor.org/rfc/rfc8032.html#section-7.1)
