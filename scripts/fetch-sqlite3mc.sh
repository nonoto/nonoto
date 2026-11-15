#!/bin/sh
# SQLite3 Multiple Ciphers (sqlite3mc) のamalgamationを取得し、チェックサムを確かめて展開する。
# spikes/sqlite3mc-comparison のビルドが、展開したソースを使う。
#
# 使い方: scripts/fetch-sqlite3mc.sh
# 展開済みでチェックサムも一致していれば、何もしない。
set -eu

version=2.7.0
sqlite_version=3.54.1
archive="sqlite3mc-$version-sqlite-$sqlite_version-amalgamation.zip"
# 配布元のSHA256SUMSに載っている値。版を上げるときは一緒に書き換える。
checksum=ba563ea5407ee07d9a0331bff5e046893b69ace6f6222030c86826a851795f30
url="https://github.com/utelle/SQLite3MultipleCiphers/releases/download/v$version/$archive"

root=$(cd "$(dirname "$0")/.." && pwd)
dest="$root/target/sqlite3mc/$version"

sha256() {
    if command -v sha256sum > /dev/null 2>&1; then
        sha256sum "$1" | cut -d ' ' -f 1
    else
        shasum -a 256 "$1" | cut -d ' ' -f 1
    fi
}

if [ -f "$dest/$archive" ] && [ "$(sha256 "$dest/$archive")" = "$checksum" ] \
    && [ -f "$dest/sqlite3mc_amalgamation.c" ]; then
    exit 0
fi

mkdir -p "$dest"
curl --fail --silent --show-error --location --output "$dest/$archive.part" "$url"
actual=$(sha256 "$dest/$archive.part")
if [ "$actual" != "$checksum" ]; then
    rm -f "$dest/$archive.part"
    echo "sqlite3mcのチェックサムが一致しない: $actual" >&2
    exit 1
fi
mv "$dest/$archive.part" "$dest/$archive"
unzip -o -q "$dest/$archive" -d "$dest"
