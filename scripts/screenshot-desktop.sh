#!/bin/sh
# 仮想ディスプレイでデスクトップアプリを起動し、画面を画像に保存する。
# 見本のページを入れた一時的なデータの置き場を使い、手元のデータには触れない。
#
# 使い方: scripts/screenshot-desktop.sh [保存先のPNG]
# 環境変数 SCREENSHOT_WAIT で、起動してから保存するまでの秒数を変えられる。
set -eu

out=${1:-tmp/screenshot-desktop.png}
wait=${SCREENSHOT_WAIT:-4}
fixtures=crates/nonoto-desktop/fixtures

data_dir=$(mktemp -d)
trap 'rm -rf "$data_dir"' EXIT
export NONOTO_DATA_DIR="$data_dir"
export NONOTO_KEY_STORE=file

cargo build --quiet --locked -p nonoto-cli -p nonoto-desktop

# 一覧は更新した日時の新しい順に並ぶため、最初に表示したいページを最後に作る。
target/debug/nonoto create --title "議事録 11月12日" --body-file "$fixtures/minutes.md" > /dev/null
target/debug/nonoto create --title "はじめに" --body-file "$fixtures/welcome.md" > /dev/null

mkdir -p "$(dirname "$out")"
# 画面の大きさをウィンドウの大きさに合わせ、画面全体を保存する。
xvfb-run --auto-servernum --server-args="-screen 0 1024x720x24" sh -eu -c '
    target/debug/nonoto-desktop &
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
' sh "$wait" "$out"

echo "$out"
