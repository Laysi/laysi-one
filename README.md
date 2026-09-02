# Laysi 授權伺服器

經銷商與代理商在這裡開立啟動金鑰，Laysi One 的每一個安裝在這裡定期報到。

```bash
docker compose up -d --build
docker compose exec app laysi-license create-vendor you@example.com   # 第一個帳號
docker compose exec app laysi-license public-key                      # 編進產品的公鑰
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
cargo test                                   # 不需要資料庫
DATABASE_URL=postgres://… LICENSE_SIGNING_KEY=$(openssl rand -hex 32) cargo run
```
