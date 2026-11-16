#!/bin/sh
# Fetch the public NASDAQ TotalView-ITCH 5.0 sample of 2019-01-30 into data/itch.
#
#   sh tools/fetch-itch.sh            # the whole day: 4.8 GB compressed, 11.2 GB unpacked
#   PREFIX_MB=30 sh tools/fetch-itch.sh   # only the first 30 MB of the compressed file
#                                         # (the start of the day, enough for the checks in CI)
set -eu
cd "$(dirname "$0")/.."
URL="https://emi.nasdaq.com/ITCH/Nasdaq%20ITCH/01302019.NASDAQ_ITCH50.gz"
mkdir -p data/itch
if [ -n "${PREFIX_MB:-}" ]; then
  OUT=data/itch/prefix.NASDAQ_ITCH50
  curl -sSfL --retry 3 -A "Mozilla/5.0" -r 0-$((PREFIX_MB * 1048576 - 1)) -o data/itch/prefix.gz "$URL"
  # the compressed stream is cut off: gzip complains at the end, the data before is fine
  gzip -dc data/itch/prefix.gz > "$OUT" 2>/dev/null || true
  rm -f data/itch/prefix.gz
else
  OUT=data/itch/01302019.NASDAQ_ITCH50
  curl -sSfL --retry 3 -A "Mozilla/5.0" -C - -o data/itch/01302019.NASDAQ_ITCH50.gz "$URL"
  test "$(wc -c < data/itch/01302019.NASDAQ_ITCH50.gz)" -eq 4764426091
  gzip -dc data/itch/01302019.NASDAQ_ITCH50.gz > "$OUT"
fi
ls -l "$OUT"
