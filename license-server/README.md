# Laysi 授權伺服器

經銷商與代理商在這裡開立啟動金鑰，Laysi One 的每一個安裝在這裡定期報到。

```bash
docker compose up -d --build
docker compose exec app laysi-one-license create-vendor you@example.com   # 第一個帳號
docker compose exec app laysi-one-license public-key                      # 編進產品的公鑰
open http://localhost:8090
```

## 兩個介面

- `POST /v1/license/check` — 安裝端唯一會呼叫的端點。第一次呼叫即為啟用並綁定主機，
  之後每一次都是續期。
- 經銷商後台 — 開立、續約、解除綁定、暫停、月結對帳。代理商看得到自己簽的經銷商。

## 三個不變量

1. **私鑰只在這裡。** 產品帶的是公鑰，所以斷網不影響營運，改客戶自己的資料庫也偽造不出授權。
2. **一組金鑰綁一台。** 第一次檢查記下主機識別碼，不同識別碼一律拒絕並記錄，換主機由
   經銷商在後台按「解除綁定」。
3. **到期是唯讀，不是鎖死。** 帳是客戶的：授權失效之後所有讀取、報表與匯出照常，只有寫入停下來。

## 開發

```bash
bun scripts/dev.ts                           # podman 起一個 5433 的資料庫，接著 cargo watch
bun scripts/dev.ts stop                      # 停資料庫，資料留著
bun scripts/dev.ts reset                     # 清空資料庫，下次啟動重建
cargo test                                   # 不需要資料庫
```

第一次啟動會產生 `.dev/license_signing_key`（開發專用，與 `secrets/` 的正式金鑰無關），
並建立後台帳號 `dev@laysi.one`，密碼印在畫面上也寫進 `.dev/vendor.txt`。整個 `.dev/`
都是拋棄式的，刪掉再跑就是一組新的。

## 部署

正式站是 `https://license.laysi.one`。容器只講純 HTTP、只綁 loopback，TLS 與公開名稱
交給前面的通道（與 Laysi One 自己的部署同一套）。伺服器讀 `X-Forwarded-Proto` 決定
session cookie 的 `Secure`、讀 `X-Forwarded-For` 記錄來源位址，所以通道要把兩個標頭
帶進來。

```bash
git clone <repo> ~/laysi-one-license && cd ~/laysi-one-license
# 把備份的簽章私鑰先放進資料卷，不要讓 secrets 服務自己抽一把新的：
#   換一把金鑰＝現場每一個安裝的授權檔全部作廢
docker volume create laysi-one-license_secrets
docker run --rm -v laysi-one-license_secrets:/s -v "$PWD:/in" alpine \
  sh -c 'cp /in/license_signing_key /s/ && chmod 0444 /s/license_signing_key'

docker compose up -d --build
docker compose exec app laysi-one-license create-vendor you@example.com
```

已經用舊名 `laysi-license` 跑過的主機：compose 專案名現在寫死在檔案裡，volume 前綴
跟著換成 `laysi-one-license_*`。升級前先把舊 volume 的 `license_signing_key` 取出來，
照上面那段放進新的 volume——直接 `up` 等於抽一把新金鑰。

產品端的 `ops/license.rs` 把 `license.laysi.one` 寫成常數，公鑰也編在裡面——**換網址或
換金鑰都是發一版新映像**，不是改設定。
