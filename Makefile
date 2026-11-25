# Windows向けのビルドに使うターゲット。開発コンテナにMinGWを入れている。
WINDOWS_TARGET := x86_64-pc-windows-gnu

# ワークスペースの外に置く、sqlite3mcにリンクするクレート。設定 (.cargo/config.toml) を読ませるため、このディレクトリでcargoを実行する。
SQLITE3MC_CRATE := spikes/sqlite3mc-comparison

.PHONY: fmt
fmt:
	cargo fmt --all
	cargo fmt --manifest-path $(SQLITE3MC_CRATE)/Cargo.toml

.PHONY: fmt-check
fmt-check:
	cargo fmt --all --check
	cargo fmt --manifest-path $(SQLITE3MC_CRATE)/Cargo.toml --check

.PHONY: lint
# `build.warnings` で、ローカルのパッケージに対するrustc・clippy・Cargoのリントの警告をエラーにする。
# 作業中のビルドやテストを警告で止めないよう、リントのコマンドとCIのclippyに設定を渡す。
lint: fetch-sqlite3mc
	cargo clippy --workspace --all-targets --locked --config 'build.warnings="deny"'
	cd $(SQLITE3MC_CRATE) && cargo clippy --all-targets --locked --config 'build.warnings="deny"'

.PHONY: test
test: fetch-sqlite3mc
	cargo test --workspace --locked
	cd $(SQLITE3MC_CRATE) && cargo test --locked

# テストを RUNS 回繰り返し、失敗した回の出力を tmp/repeat-test/ に残す。
# 繰り返すテストは REPEAT_TEST (cargo testの引数) で変えられる。既定は、プロセスで初めてストアを同時に開く再現のテスト。
# 同時に開くスレッドの数は NONOTO_OPEN_THREADS で変えられる (既定は16)。
RUNS ?= 300
REPEAT_TEST ?= -p nonoto-core --test concurrent_open -- --ignored --exact プロセスで初めて別々のファイルのストアを同時に開ける

.PHONY: repeat-test
repeat-test:
	scripts/repeat-test.sh $(RUNS) $(REPEAT_TEST)

# sqlite3mcのソースを取得する。取得済みなら何もしない。
.PHONY: fetch-sqlite3mc
fetch-sqlite3mc:
	scripts/fetch-sqlite3mc.sh

.PHONY: build-core-windows
build-core-windows:
	cargo build -p nonoto-core --target $(WINDOWS_TARGET) --locked

# 全文検索のトークナイザーを登録する計測のコードを、Windows向けにビルドする。
.PHONY: build-store-comparison-windows
build-store-comparison-windows:
	cargo build -p store-comparison --target $(WINDOWS_TARGET) --locked

# CRDTのライブラリを比べるコードを、Windows向けにビルドする。
.PHONY: build-crdt-comparison-windows
build-crdt-comparison-windows:
	cargo build -p crdt-comparison --target $(WINDOWS_TARGET) --locked

# 暗号の部品の比較のコードを、Windows向けにビルドする。
.PHONY: build-crypto-comparison-windows
build-crypto-comparison-windows:
	cargo build -p crypto-comparison --target $(WINDOWS_TARGET) --locked

# sqlite3mcにリンクする比較のコードを、Windows向けにビルドする。
.PHONY: build-sqlite3mc-windows
build-sqlite3mc-windows: fetch-sqlite3mc
	cd $(SQLITE3MC_CRATE) && cargo build --target $(WINDOWS_TARGET) --locked

# sqlite3mcとSQLCipherで、ページの保存・起動・メモリ・1件の保存と更新を同じ条件で計り、表に並べる。
# 件数は MEASURE_PAGES で変えられる。SQLCipher版は、sqlite3mc版と成果物を分けてビルドする。
MEASURE_PAGES ?= 10000

.PHONY: measure-sqlite3mc
measure-sqlite3mc: fetch-sqlite3mc
	cd $(SQLITE3MC_CRATE) && cargo build --release --locked
	cd $(SQLITE3MC_CRATE) && cargo build --release --locked --features sqlcipher --target-dir ../../target/sqlite3mc-comparison-sqlcipher
	target/sqlite3mc-comparison/release/sqlite3mc-comparison --pages $(MEASURE_PAGES) \
		--baseline target/sqlite3mc-comparison-sqlcipher/release/sqlite3mc-comparison

# CRDTのライブラリの候補を、同じ入力の列で作った文書で計測する。行数は LINES で変えられる (既定は5000)。
.PHONY: measure-crdt-comparison
measure-crdt-comparison:
	cargo run --release --locked -p crdt-comparison -- --lines $(LINES)

# 暗号の部品の系統 (RustCrypto・libcrux) に、必須の条件を当て、同じ操作で計測する。回数は CRYPTO_RUNS で変えられる (既定は1000)。
CRYPTO_RUNS ?= 1000

.PHONY: measure-crypto-comparison
measure-crypto-comparison:
	@cargo run --release --locked -p crypto-comparison -- --runs $(CRYPTO_RUNS)

# 開発コンテナの仮想ディスプレイでデスクトップアプリを起動し、画面を tmp/screenshot-desktop.png に保存する。
.PHONY: screenshot-desktop
screenshot-desktop:
	scripts/screenshot-desktop.sh

# 仮想ディスプレイで記法の見本のページの編集の画面を開き、ライトとダークの画面を
# tmp/screenshot-editor-light.png と tmp/screenshot-editor-dark.png に保存する。
.PHONY: screenshot-editor
screenshot-editor:
	scripts/screenshot-editor.sh

# 数千行のMarkdownのページを1件作る。行数は LINES で変えられる (例: make seed-large-page LINES=10000)。
LINES ?= 5000

.PHONY: seed-large-page
seed-large-page:
	scripts/seed-large-page.sh $(LINES)

# 補完の検索を確かめる短いページをまとめて作る。データの置き場はNONOTO_DATA_DIRで指定する。
PAGES ?= 10000

.PHONY: seed-pages
seed-pages:
	cargo run --quiet --locked -p nonoto-cli --example seed-pages -- --count $(PAGES)
