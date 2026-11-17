<!-- last_synced: 2026-11-16 -->

# Nonoto

> 日本語 | [English](./README.en.md)

Nonotoは、正本を手元の端末に置くローカルファーストなドキュメント共有サービスです。
今は、構成が成り立つかを確かめる段階にあります。

## 構成

| ディレクトリ                  | 内容                                                           |
| ----------------------------- | -------------------------------------------------------------- |
| `crates/nonoto-core`          | ページの操作・端末上の暗号化ストア・鍵の保管                   |
| `crates/nonoto-cli`           | コマンド `nonoto` (ページの作成・一覧・取得・更新。出力はJSON) |
| `crates/nonoto-desktop`       | GPUIのデスクトップアプリ                                       |
| `spikes/store-comparison`     | 暗号化ストアの方式を比べるコード                               |
| `spikes/sqlite3mc-comparison` | SQLCipherとsqlite3mcを比べるコード (ワークスペースの外)        |

## 開発コンテナでの開発

`Dockerfile.dev` の開発コンテナで、実装と自動の確認を行います。

```sh
make fmt                             # 整形する
make lint                            # clippyで検査する
make test                            # テストを実行する
make build-core-windows              # Windows向けにコアをビルドする
make build-store-comparison-windows  # Windows向けに暗号化ストアの計測のコードをビルドする
make build-sqlite3mc-windows         # Windows向けにsqlite3mcの比較のコードをビルドする
make measure-sqlite3mc               # sqlite3mcとSQLCipherを同じ条件で計測する
make screenshot-desktop              # 仮想ディスプレイでデスクトップアプリを起動し、画面を tmp/ に保存する
make screenshot-editor               # 記法の見本のページを編集の画面で開き、ライトとダークの画面を tmp/ に保存する
make repeat-test                     # テストを繰り返し、失敗した回の出力を tmp/repeat-test/ に残す
```

データの置き場と鍵の保管場所は、コマンド `nonoto` とデスクトップアプリで共通の環境変数で変えられます。

| 環境変数           | 内容                                                                                                |
| ------------------ | --------------------------------------------------------------------------------------------------- |
| `NONOTO_DATA_DIR`  | データの置き場。省略するとOSの慣習に沿った置き場 (macOSでは `~/Library/Application Support/nonoto`) |
| `NONOTO_KEY_STORE` | 鍵の保管場所 (`keychain` か `file`)。省略するとmacOSとWindowsではキーチェーン、それ以外ではファイル |

## テストを繰り返して確かめる

まれにしか起きない失敗は、`make repeat-test` でテストをプロセスごと繰り返して頻度を見ます。
失敗した回の出力と、条件と結果のまとめ (`summary.txt`) を `tmp/repeat-test/<日時>-<識別子>/` に残し、1回でも失敗すると終了コード1で終わります。

```sh
make repeat-test                                                    # ストアを同時に開く再現のテストを300回繰り返す
make repeat-test RUNS=1000 NONOTO_OPEN_THREADS=64                   # 回数と同時に開くスレッドの数を変える
make repeat-test REPEAT_TEST="-p nonoto-core --lib store::sqlcipher"  # 繰り返すテストを cargo test の引数で変える
```

`make test` も、ストアを同時に開く再現のテストを、子のプロセスとして60回繰り返します (1回に64スレッド)。
回数は `NONOTO_OPEN_PROCESSES` で変えられます (例: `make test NONOTO_OPEN_PROCESSES=10`)。

## 暗号化ストアを計測する

暗号化ストアの方式と、方式Aの全文検索のインデックスの変種 (部分一致の走査・trigram・バイグラム・ユニグラム) を、同じページで計測します。

```sh
cargo run --release --locked -p store-comparison -- --pages 10000
```

| 引数      | 内容                                                                             |
| --------- | -------------------------------------------------------------------------------- |
| `--pages` | ページの件数。省略すると10000                                                    |
| `--dir`   | データベースを置くディレクトリ。省略すると一時ディレクトリを使い、終わったら消す |

結果はMarkdownで標準出力に書きます。
最後の「必須の条件」の表に、変種ごとに条件を満たしたかを、「計測の値」の表に主な値を示します。
ページは固定した種の乱数で作るため、実行のたびに同じ内容になります。
ページを1件ずつ保存するため、開発コンテナでは1万ページで7分ほど、5万ページで30分ほどかかります。

### 暗号化の実装を比べる

`spikes/sqlite3mc-comparison` は、OpenSSLに依存しない暗号化の実装 (SQLite3 Multiple Ciphers。以下sqlite3mc) とSQLCipherを、同じストアのコードで比べます。
ワークスペースのSQLCipherとsqlite3mcの両方には `rusqlite` をリンクできないため、ワークスペースの外に置いています。
cargoは、このクレートのディレクトリで実行します (クレートの `.cargo/config.toml` を読ませるため)。

sqlite3mcのソースはリポジトリに置かず、`make fetch-sqlite3mc` がチェックサムを確かめて `target/sqlite3mc/` に取得します。
`make lint`・`make test`・`make build-sqlite3mc-windows`・`make measure-sqlite3mc` は、取得していなければ先に取得します。

```sh
make measure-sqlite3mc                     # 1万ページで計測する
make measure-sqlite3mc MEASURE_PAGES=1000  # 件数を変えて計測する
```

sqlite3mc版とSQLCipher版 (機能 `sqlcipher`) をビルドし、同じ件数で計って、全件の保存・起動・起動で増えたメモリ・1件の保存と更新・ファイルの大きさを1つの表に並べます。
SQLCipher版は、OpenSSLを同梱してビルドするため、初めてのビルドに数分かかります。

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

普段のデータと分けた置き場を指定して、補完の検索に使う短いページ1万件と、5000行のページを入れます。

```sh
export NONOTO_DATA_DIR=/tmp/nonoto-editor-check

# 短いページをまとめて1万件作る (件数は PAGES で変えられる)
make seed-pages PAGES=10000

# 数千行のMarkdownのページを1件作る (行数は LINES で変えられる)
make seed-large-page LINES=5000

# 表示のWikiリンクのリンク先になるページを作る
cargo run --release -p nonoto-cli -- create --title はじめに --body '# 見出し'
```

`make seed-pages` は、鍵の取得とデータベースを開く処理を一度だけにして、既存のコアのAPIで指定した件数のページを追加します。
鍵の保管場所は、アプリと同じく `NONOTO_KEY_STORE` で指定でき、省略時も同じ既定値を使います。
アプリを開いたまま入れたときは、アプリを起動し直すと一覧に出ます。

## エディタを確かめる

データを入れたのと同じターミナルで `cargo run --release -p nonoto-desktop` を起動し、同じ置き場のデータを開いて確かめます。

### 大きなページ

「大きなページ (5000行)」を開いて「編集」を押し、文字を打ち続けたり、上下に素早くスクロールしたりして、引っかからないかを見ます。

### Wikiリンクの補完

本文で `[[` を打つと、タイトルの候補が出ます。
`[[は` のように日本語で続けて打ち、変換中の文字が崩れず、確定したあとに候補が絞られるかを見ます。
候補はEnterで選びます。

### 1万件のページでの補完

本文で `[[検証用ページ` を日本語で入力し、候補が出るかを見ます。
続けて ` 000` のように打って絞り込み、上下で候補を動かしてEnterで選びます。
1万件以上のページがある状態でも、文字の入力と候補の更新が引っかからないかを確かめます。

### リストの継続

`- 項目` の行の末尾でEnterを押すと、次の行に `- ` が入ります。
何も書かずにもう一度Enterを押すと、記号が消えます。
変換を確定するEnterでは、リストが続かないかを確かめます。

### 字下げ

複数の行を選んでTabとShift-Tabを押し、選んだ行の字下げが変わるかを見ます。

### 部品の機能

`(` を打つと `)` が入ります。
Command-Fで本文を検索でき、Command-ZとCommand-Shift-Zで取り消しとやり直しができるかを確かめます。

### 表示のWikiリンク

本文に `[[はじめに]]` と `[[無いページ]]` を書いて保存し、表示で `はじめに` を押すと「はじめに」のページへ移ります。
`[[無いページ]]` はリンクにならずに括弧も含めて書いたとおりに表示され、押しても移らないかを確かめます。

### VoiceOver

Command-F5でVoiceOverを起動し、編集中の本文の入力へ移って、本文が読み上げられるかを聞きます。

## 後片付け

`NONOTO_DATA_DIR` で指定した置き場は、ディレクトリごと消せます。
キーチェーンの項目を消すと、その鍵で暗号化したデータは読めなくなります。
消すのは、データの置き場も一緒に消すときだけにしてください。
