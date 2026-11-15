# Windows向けのコアのビルドに使うターゲット。開発コンテナにMinGWを入れている。
WINDOWS_TARGET := x86_64-pc-windows-gnu

.PHONY: fmt
fmt:
	cargo fmt --all

.PHONY: fmt-check
fmt-check:
	cargo fmt --all --check

.PHONY: lint
lint:
	cargo clippy --workspace --all-targets --locked -- -D warnings

.PHONY: test
test:
	cargo test --workspace --locked

.PHONY: build-core-windows
build-core-windows:
	cargo build -p nonoto-core --target $(WINDOWS_TARGET) --locked

# 全文検索のトークナイザーを登録する計測のコードを、Windows向けにビルドする。
.PHONY: build-store-comparison-windows
build-store-comparison-windows:
	cargo build -p store-comparison --target $(WINDOWS_TARGET) --locked

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
