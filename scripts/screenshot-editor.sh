#!/bin/sh
# 仮想ディスプレイで、記法の見本のページの編集の画面を開き、ライトとダークの画面を画像に保存する。
# 見本のページを入れた一時的なデータの置き場を使い、手元のデータには触れない。
#
# 使い方: scripts/screenshot-editor.sh [保存先のディレクトリ]
# 画像は <保存先>/screenshot-editor-light.png と screenshot-editor-dark.png に保存する。
# 環境変数 SCREENSHOT_WAIT で、起動してから保存するまでの秒数を変えられる。
set -eu

out_dir=${1:-tmp}
wait=${SCREENSHOT_WAIT:-6}

data_dir=$(mktemp -d)
trap 'rm -rf "$data_dir"' EXIT
export NONOTO_DATA_DIR="$data_dir"
export NONOTO_KEY_STORE=file

cargo build --quiet --locked -p nonoto-cli
cargo build --quiet --locked -p nonoto-desktop --example editor-screenshot

target/debug/nonoto create --title "記法の見本" --body-file crates/nonoto-desktop/fixtures/markdown-syntax.md > /dev/null

mkdir -p "$out_dir"
for appearance in light dark; do
    out="$out_dir/screenshot-editor-$appearance.png"
    # 画面の大きさをウィンドウの大きさに合わせ、画面全体を保存する。
    xvfb-run --auto-servernum --server-args="-screen 0 1024x1440x24" sh -eu -c '
        target/debug/examples/editor-screenshot "$3" &
        pid=$!
        trap '\''kill "$pid" 2> /dev/null || :; wait "$pid" 2> /dev/null || :'\'' EXIT
        trap '\''exit 130'\'' INT
        trap '\''exit 143'\'' TERM
        sleep "$1"
        if ! kill -0 "$pid" 2> /dev/null; then
            echo "デスクトップアプリが保存の前に終了した" >&2
            exit 1
        fi
        import -window root "$2"
    ' sh "$wait" "$out" "$appearance"
    echo "$out"
done
