#!/bin/sh
# 数千行のMarkdownのページを1件作る。入力とスクロールが引っかからないかを確かめるために使う。
#
# 使い方: scripts/seed-large-page.sh [行数]
# データの置き場と鍵の保管場所は、コマンド nonoto と同じく NONOTO_DATA_DIR・NONOTO_KEY_STORE で変えられる。
set -eu

lines=${1:-5000}
case $lines in
    '' | 0 | *[!0-9]*)
        echo "行数は正の整数で指定する: $lines" >&2
        exit 1
        ;;
esac

# 見出し・箇条書き・空行・長めの日本語の文を混ぜる。
awk -v lines="$lines" 'BEGIN {
    for (i = 1; i <= lines; i++) {
        if (i % 100 == 1) {
            printf "## 節%d\n", int(i / 100) + 1
        } else if (i % 10 == 0) {
            print ""
        } else if (i % 7 == 0) {
            printf "- 箇条書きの項目%d\n", i
        } else {
            printf "%d行目: 日本語の文章を書いて、入力とスクロールが引っかからないかを確かめる。\n", i
        }
    }
}' | cargo run --quiet --locked -p nonoto-cli -- create --title "大きなページ (${lines}行)" --body-file - > /dev/null

echo "ページを作った: 大きなページ (${lines}行)"
