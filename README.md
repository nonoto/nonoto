<!-- last_synced: 2026-11-13 -->

# Nonoto

> 日本語 | [English](./README.en.md)

Nonotoは、正本を手元の端末に置くローカルファーストなドキュメント共有サービスです。
今は、構成が成り立つかを確かめる段階にあります。

## 構成

| ディレクトリ               | 内容                                                         |
| -------------------------- | ------------------------------------------------------------ |
| `crates/nonoto-core`       | ページの操作・端末上の暗号化ストア・鍵の保管                 |
| `crates/nonoto-cli`        | コマンド `nonoto` (ページの作成・一覧・取得・更新。出力はJSON) |
| `crates/nonoto-desktop`    | GPUIのデスクトップアプリ                                     |
| `spikes/store-comparison`  | 暗号化ストアの方式を比べるコード                             |

## 開発コンテナでの開発

`Dockerfile.dev` の開発コンテナで、実装と自動の確認を行います。

```sh
make fmt                 # 整形する
make lint                # clippyで検査する
make test                # テストを実行する
make build-core-windows  # Windows向けにコアをビルドする
make screenshot-desktop  # 仮想ディスプレイでデスクトップアプリを起動し、画面を tmp/ に保存する
```

データの置き場と鍵の保管場所は、コマンド `nonoto` とデスクトップアプリで共通の環境変数で変えられます。

| 環境変数           | 内容                                                                                         |
| ------------------ | -------------------------------------------------------------------------------------------- |
| `NONOTO_DATA_DIR`  | データの置き場。省略するとOSの慣習に沿った置き場 (macOSでは `~/Library/Application Support/nonoto`) |
| `NONOTO_KEY_STORE` | 鍵の保管場所 (`keychain` か `file`)。省略するとmacOSとWindowsではキーチェーン、それ以外ではファイル |

## Macで確かめる

日本語の入力 (IME) やVoiceOver、キーチェーンのように、Macでしか確かめられないことは手元のMacで確かめます。

### 準備

1. App StoreからXcodeを入れ、一度起動してライセンスに同意する
2. 使うXcodeを切り替える

   ```sh
   sudo xcode-select -s /Applications/Xcode.app/Contents/Developer
   ```

3. [mise](https://mise.jdx.dev/) を入れ、リポジトリのルートで `mise.toml` のRustを入れる

   ```sh
   mise install rust
   ```

`xcrun -f metal` がパスを返せば、デスクトップアプリをビルドできます。
ビルドで `metal` が見つからないと言われたら、`xcodebuild -downloadComponent MetalToolchain` で入れてください。

### ブランチを取得する

開発コンテナで作ったブランチは、開発環境のリポジトリをリモートに足して取得します。

```sh
git remote add dev <開発環境のリポジトリのURL>
git fetch dev
git switch --track dev/<ブランチ名>
```

### ビルドして起動する

```sh
cargo run --release -p nonoto-desktop
```

初回のビルドには十数分かかります。
初めて起動すると、鍵がキーチェーンに `nonoto` という名前の項目で保存されます。

普段のデータと分けて試すときは、データの置き場を指定します。

```sh
export NONOTO_DATA_DIR=/tmp/nonoto-mac
cargo run --release -p nonoto-desktop
```

### テスト用のデータを入れる

起動するときと同じ環境変数で、データを入れます。

```sh
# 数千行のMarkdownのページを1件作る (行数は LINES で変えられる)
make seed-large-page LINES=5000

# 短いページを作る
cargo run --release -p nonoto-cli -- create --title はじめに --body '# 見出し'
```

アプリを開いたまま入れたときは、アプリを起動し直すと一覧に出ます。

### 後片付け

`NONOTO_DATA_DIR` で指定した置き場は、ディレクトリごと消せます。
キーチェーンの項目を消すと、その鍵で暗号化したデータは読めなくなります。
消すのは、データの置き場も一緒に消すときだけにしてください。
