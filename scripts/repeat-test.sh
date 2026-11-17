#!/bin/sh
# テストを指定した回数だけ繰り返し、失敗した回の出力を残す。まれにしか起きない失敗の頻度を見るために使う。
#
# 使い方: scripts/repeat-test.sh <回数> <cargo testの引数...>
# 例: scripts/repeat-test.sh 300 -p nonoto-core --lib store::sqlcipher
#
# 出力は tmp/repeat-test/<日時>-<識別子>/ に置く。summary.txt に条件と結果を、failed-<回>.txt に失敗した回の出力を書く。
# 1回でも失敗すると、終了コード1で終わる。
set -eu

if [ $# -lt 1 ]; then
    echo "使い方: $0 <回数> <cargo testの引数...>" >&2
    exit 2
fi

runs=$1
shift
case $runs in
    '' | 0 | *[!0-9]*)
        echo "回数は正の整数で指定する: $runs" >&2
        exit 2
        ;;
esac

mkdir -p tmp/repeat-test
out=$(mktemp -d "tmp/repeat-test/$(date +%Y%m%d-%H%M%S)-XXXXXX")

# 繰り返しの時間にビルドを含めないよう、先にビルドしておく。
cargo test --locked --no-run "$@" > "$out/build.txt" 2>&1 || {
    cat "$out/build.txt" >&2
    exit 1
}

started=$(date +%s)
failed=0
i=1
while [ "$i" -le "$runs" ]; do
    if ! cargo test --locked --quiet "$@" > "$out/run.txt" 2>&1; then
        failed=$((failed + 1))
        mv "$out/run.txt" "$out/failed-$i.txt"
    fi
    i=$((i + 1))
done
rm -f "$out/run.txt"
elapsed=$(($(date +%s) - started))

# テストが読む条件 (スレッドの数・SQLCipherのログの水準など) を、名前を列挙せずにすべて残す。
# 該当が無いと grep が終了コード1を返すため、set -e で止まらないよう、パイプの最後を sed にする。
vars=$(env | grep '^NONOTO_' | sort | sed 's/^/  /')

{
    echo "cargo testの引数: $*"
    echo "環境変数 (NONOTO_で始まるもの):"
    echo "${vars:-  無し}"
    echo "回数: $runs"
    echo "失敗: $failed"
    echo "時間: ${elapsed}秒"
} > "$out/summary.txt"

cat "$out/summary.txt"
echo "出力: $out"
[ "$failed" -eq 0 ]
