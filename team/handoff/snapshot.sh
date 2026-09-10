#!/bin/sh
# СНЯТЬ БАЙТЫ — запускает владелец репозитория со своей машины.
#
# Агенты этого сделать не могут: из их песочницы все восемь площадок отвечают
# 000 (устав, правило 1a). Этот скрипт — единственный способ, которым в проект
# попадают настоящие измеренные байты вместо вспомненных.
#
#   sh team/handoff/snapshot.sh
#
# Кладёт ответы в team/handoff/bytes/<ISO-время>/ по файлу на запрос, плюс
# MANIFEST.txt с кодом ответа и размером каждого. Ничего не перезаписывает:
# каждый прогон — своя папка, потому что снимок книги живёт секунды.
#
# ЧТО ПОТОМ: скажи агенту ENG-LIVE «байты в team/handoff/bytes/<папка>».
# Он вкомпилирует их в разделы captured и пометит Provenance::LiveMeasured.
#
# ---------------------------------------------------------------------------
# ЧЕСТНО ПРО ЭТОТ ФАЙЛ, потому что жёсткое правило 8 обязывает:
#
# [assumption] Адреса эндпоинтов ниже — по памяти об API этих площадок, они НЕ
# проверены. Часть может отвечать 404 или переехать. Скрипт написан так, что
# это будет ВИДНО: код ответа пишется в манифест, а тело сохраняется как есть,
# даже если это ошибка. Пустой или 404-й файл — не поломка скрипта, а факт про
# эндпоинт, и агент обязан прочитать его как факт, а не выдумать содержимое.
#
# [fact, verified] Флаги curl совпадают с decisions/027: только https,
# редиректы не следуются, --insecure отсутствует. Ослабление TLS здесь не
# появляется по той же причине, по которой его нет в arb-live.
# ---------------------------------------------------------------------------

set -u
cd "$(dirname "$0")/../.." || exit 2

STAMP=$(date -u +%Y-%m-%dT%H-%M-%SZ)
OUT="team/handoff/bytes/$STAMP"
mkdir -p "$OUT" || exit 2
MAN="$OUT/MANIFEST.txt"

# Считаются в get(), а не грепом по манифесту. Прежняя редакция брала
# `wc -l` минус 8 строк шапки, а шапки было 9 — и скрипт доложил «33 из 34»,
# то есть выдумал тридцать четвёртый запрос. Счётчик соврать не может: он
# растёт там же, где выполняется запрос.
N_REQ=0
N_OK=0

{
  echo "СНИМОК БАЙТОВ"
  echo "снят:    $STAMP (UTC)"
  echo "машина:  $(uname -sm)"
  echo "curl:    $(curl --version 2>/dev/null | head -1)"
  echo
  echo "Провенанс: измерено живьём с машины владельца. Это те байты, которые"
  echo "имеют право стать Provenance::LiveMeasured."
  echo
  printf '%-42s %6s %10s  %s\n' ФАЙЛ КОД БАЙТ URL
} > "$MAN"

# get <имя-файла> <url>
get() {
  name=$1; url=$2
  code=$(curl -s \
      --proto '=https' --proto-redir '=https' --tlsv1.2 \
      --max-time 20 --max-filesize 8000000 \
      -H 'Accept: application/json' \
      -o "$OUT/$name" -w '%{http_code}' \
      "$url" 2>/dev/null)
  rc=$?
  [ -f "$OUT/$name" ] || : > "$OUT/$name"
  size=$(wc -c < "$OUT/$name" | tr -d ' ')
  [ "$rc" -ne 0 ] && code="curl:$rc"
  N_REQ=$((N_REQ+1))
  printf '%-42s %6s %10s  %s\n' "$name" "$code" "$size" "$url" >> "$MAN"
  if [ "$code" = "200" ] && [ "$size" -gt 2 ]; then
    N_OK=$((N_OK+1))
    printf '  \033[32m%6s\033[0m %10s  %s\n' "$code" "$size" "$name"
  else
    printf '  \033[31m%6s\033[0m %10s  %s\n' "$code" "$size" "$name"
  fi
}

echo
echo "Пишу в $OUT"
echo

# --------------------------------------------------------------- BYBIT
echo "BYBIT"
B=https://api.bybit.com
get bybit-instruments.json "$B/v5/market/instruments-info?category=linear&symbol=BTCUSDT"
get bybit-tickers.json     "$B/v5/market/tickers?category=linear&symbol=BTCUSDT"
get bybit-orderbook.json   "$B/v5/market/orderbook?category=linear&symbol=BTCUSDT&limit=5"
get bybit-funding-hist.json "$B/v5/market/funding/history?category=linear&symbol=BTCUSDT&limit=30"

# --------------------------------------------------------------- BINANCE
echo "BINANCE"
B=https://fapi.binance.com
get binance-exchangeinfo.json "$B/fapi/v1/exchangeInfo"
get binance-premiumindex.json "$B/fapi/v1/premiumIndex?symbol=BTCUSDT"
get binance-depth.json        "$B/fapi/v1/depth?symbol=BTCUSDT&limit=5"
get binance-funding-hist.json "$B/fapi/v1/fundingRate?symbol=BTCUSDT&limit=30"
get binance-funding-info.json "$B/fapi/v1/fundingInfo"

# --------------------------------------------------------------- OKX
echo "OKX"
B=https://www.okx.com
get okx-instruments.json   "$B/api/v5/public/instruments?instType=SWAP&instId=BTC-USDT-SWAP"
get okx-funding-rate.json  "$B/api/v5/public/funding-rate?instId=BTC-USDT-SWAP"
get okx-books.json         "$B/api/v5/market/books?instId=BTC-USDT-SWAP&sz=5"
get okx-funding-hist.json  "$B/api/v5/public/funding-rate-history?instId=BTC-USDT-SWAP&limit=30"

# --------------------------------------------------------------- BITGET
echo "BITGET"
B=https://api.bitget.com
get bitget-contracts.json    "$B/api/v2/mix/market/contracts?productType=USDT-FUTURES&symbol=BTCUSDT"
get bitget-fundrate.json     "$B/api/v2/mix/market/current-fund-rate?symbol=BTCUSDT&productType=USDT-FUTURES"
get bitget-orderbook.json    "$B/api/v2/mix/market/orderbook?symbol=BTCUSDT&productType=USDT-FUTURES&limit=5"
get bitget-fundrate-hist.json "$B/api/v2/mix/market/history-fund-rate?symbol=BTCUSDT&productType=USDT-FUTURES&pageSize=30"

# --------------------------------------------------------------- MEXC
echo "MEXC"
B=https://contract.mexc.com
get mexc-detail.json        "$B/api/v1/contract/detail?symbol=BTC_USDT"
get mexc-funding.json       "$B/api/v1/contract/funding_rate/BTC_USDT"
get mexc-depth.json         "$B/api/v1/contract/depth/BTC_USDT"
get mexc-funding-hist.json  "$B/api/v1/contract/funding_rate/history?symbol=BTC_USDT&page_size=30"

# --------------------------------------------------------------- GATE
echo "GATE"
B=https://api.gateio.ws
get gate-contract.json      "$B/api/v4/futures/usdt/contracts/BTC_USDT"
get gate-orderbook.json     "$B/api/v4/futures/usdt/order_book?contract=BTC_USDT&limit=5"
get gate-funding-hist.json  "$B/api/v4/futures/usdt/funding_rate?contract=BTC_USDT&limit=30"

# --------------------------------------------------------------- KUCOIN
echo "KUCOIN"
B=https://api-futures.kucoin.com
get kucoin-contract.json    "$B/api/v1/contracts/XBTUSDTM"
get kucoin-funding.json     "$B/api/v1/funding-rate/XBTUSDTM/current"
get kucoin-depth.json       "$B/api/v1/level2/depth20?symbol=XBTUSDTM"

# --------------------------------------------------------------- HTX
echo "HTX"
B=https://api.hbdm.com
get htx-contract.json       "$B/linear-swap-api/v1/swap_contract_info?contract_code=BTC-USDT"
get htx-funding.json        "$B/linear-swap-api/v1/swap_funding_rate?contract_code=BTC-USDT"
get htx-depth.json          "$B/linear-swap-ex/market/depth?contract_code=BTC-USDT&type=step0"
get htx-funding-hist.json   "$B/linear-swap-api/v1/swap_historical_funding_rate?contract_code=BTC-USDT&page_size=30"

# --------------------------------------------------------------- СПОТ
# Нужен для базиса и для того, чтобы вообще существовала нога спота.
echo "СПОТ (для базиса)"
get binance-spot-depth.json  "https://api.binance.com/api/v3/depth?symbol=BTCUSDT&limit=5"
get binance-spot-info.json   "https://api.binance.com/api/v3/exchangeInfo?symbol=BTCUSDT"

# ---------------------------------------------------------------------------
{
  echo
  echo "Итого: $N_OK из $N_REQ ответили 200."
  echo
  echo "Файл с кодом, отличным от 200, или размером в пару байт — это ФАКТ про"
  echo "эндпоинт, а не поломка. Агент обязан прочитать его как факт: адрес мог"
  echo "переехать, площадка могла закрыть эндпоинт, регион мог быть ограничен."
  echo "Выдумывать содержимое такого файла запрещено правилом 7."
} >> "$MAN"

echo
echo "Готово: $N_OK из $N_REQ ответили 200."
echo "Манифест: $MAN"
echo
# Скобки вокруг имени обязательны: `$OUT»` шелл читает как имя переменной
# вместе с первым байтом многобайтной кавычки, и под `set -u` падает с
# `OUT?: unbound variable` — уже после того, как вся работа сделана.
echo "Скажи агенту ENG-LIVE: «байты в ${OUT}»."
echo
