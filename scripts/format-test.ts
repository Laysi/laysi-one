#!/usr/bin/env bun
// The pure helpers every screen formats through, checked without a browser.
//
// These decide what a number and a timestamp look like on screen, and two of
// them do arithmetic: multiplyToDollars and addDecimals work on decimal
// strings with BigInt because the wire format is a string and a float would
// already have lost the cent. The date helpers shift to the business offset
// rather than slicing the string, which is the difference between the right
// day and the previous one for anything stamped after 16:00 UTC.
//
//   bun scripts/format-test.ts

import {
  addDecimals,
  businessClock,
  businessDate,
  businessStamp,
  businessTime,
  businessToday,
  docNumberHint,
  fileSizeLabel,
  isZero,
  maskId,
  money,
  multiplyToDollars,
  noteServerTime,
  reportAmount,
  serverClockGapMs,
  setBusinessOffset,
  signedMoney,
  trimDecimal,
  wallSpan,
} from "../web/src/api/client";

let failed = 0;

function eq(label: string, actual: unknown, expected: unknown): void {
  const ok = actual === expected;
  console.log(`${ok ? "ok  " : "FAIL"} ${label}${ok ? "" : `  得到 ${String(actual)}，應為 ${String(expected)}`}`);
  if (!ok) failed += 1;
}

function ok(label: string, condition: boolean): void {
  console.log(`${condition ? "ok  " : "FAIL"} ${label}`);
  if (!condition) failed += 1;
}

// Thousands separators, and the decimals dropped: the ledger shows dollars.
eq("千分位", money("1234567"), "1,234,567");
eq("負數保留符號", money("-1234567.89"), "-1,234,567");
eq("三位數不分節", money("999"), "999");
eq("零", money("0"), "0");

// Reports print a negative in parentheses; a delta prints its sign either way.
eq("報表負數加括號", reportAmount("-500"), "(500)");
eq("報表正數不加括號", reportAmount("500"), "500");
eq("增減正數帶加號", signedMoney("500"), "+500");
eq("增減負數帶減號", signedMoney("-500"), "-500");

// The display-layer mask. The full number is what the store keeps.
eq("身分證字號遮罩", maskId("A123456789"), "A12*****89");
eq("太短就不遮", maskId("12345"), "12345");

eq("小數尾零去掉", trimDecimal("5.10"), "5.1");
eq("整數的尾零不動", trimDecimal("80000"), "80000");
eq("整數點零收成整數", trimDecimal("80000.0000"), "80000");
ok("零的各種寫法", isZero("0") && isZero("0.00") && isZero("-0.0"));
ok("一分錢不是零", !isZero("0.01") && !isZero("10"));

// Money arithmetic on the wire format. The multiply rounds half up to whole
// dollars; the add keeps the wider of the two scales.
eq("數量乘單價", multiplyToDollars("3", "1000"), "3000");
eq("小數單價", multiplyToDollars("1.5", "100"), "150");
eq("半元進位", multiplyToDollars("0.5", "1"), "1");
eq("不足半元捨去", multiplyToDollars("0.4", "1"), "0");
eq("負數相乘", multiplyToDollars("-1.5", "2"), "-3");
eq("捨到零不寫成負零", multiplyToDollars("-0.4", "1"), "0");
// A float loses this one: 1000000000000.05 * 3 is not representable.
eq("大額不經浮點", multiplyToDollars("1000000000000.05", "3"), "3000000000000");
eq("不是數字回 null", multiplyToDollars("abc", "1"), null);
eq("空字串回 null", multiplyToDollars("", "1"), null);
eq("只有負號回 null", multiplyToDollars("-", "1"), null);

eq("小數相加", addDecimals("1.5", "2.25"), "3.75");
// The one every float gets wrong.
eq("0.1 加 0.2", addDecimals("0.1", "0.2"), "0.3");
eq("整數相加不長出小數", addDecimals("10", "5"), "15");
eq("借方減貸方跨越零", addDecimals("-0.25", "0.1"), "-0.15");
eq("相加也擋非數字", addDecimals("1", "x"), null);

eq("不足 1 MB 以 KB 進位", fileSizeLabel(2048), "2 KB");
eq("超過 1 MB 取一位小數", fileSizeLabel(3 * 1024 * 1024), "3.0 MB");

// Timestamps come back in UTC. Slicing the string would print the previous
// day for everything stamped from 16:00 UTC onwards.
setBusinessOffset("+08:00");
eq("跨日的時間戳算成當地日期", businessDate("2026-08-28T17:00:00Z"), "2026-08-29");
eq("同一天的時間戳", businessDate("2026-08-29T02:30:00Z"), "2026-08-29");
eq("沒有值就用預設字", businessDate(null), "—");
eq("預設字可以指定", businessDate(undefined, ""), "");
eq("跨日的時刻", businessTime("2026-08-28T17:00:00Z"), "08/29 01:00");
eq("完整時間戳", businessStamp("2026-08-28T17:00:00Z"), "2026-08-29 01:00:00");

// The offset is what /v1/meta says, not a constant.
setBusinessOffset("-05:00");
eq("換一個時區就換一個日期", businessDate("2026-08-29T02:00:00Z"), "2026-08-28");
setBusinessOffset("看不懂的字");
eq("看不懂的偏移量不改動現值", businessDate("2026-08-29T02:00:00Z"), "2026-08-28");
setBusinessOffset(null);
eq("沒有值也不改動現值", businessDate("2026-08-29T02:00:00Z"), "2026-08-28");
setBusinessOffset("+08:00");

// wallSpan prints the times as written, without shifting: a leave request is
// stored as the local times the person picked.
eq(
  "同一天的區間只寫一次日期",
  wallSpan("2026-08-29T09:00:00", "2026-08-29T18:00:00"),
  "2026-08-29 09:00 至 18:00",
);
eq(
  "跨日的區間兩邊都寫日期",
  wallSpan("2026-08-29T22:00:00", "2026-08-30T02:00:00"),
  "2026-08-29 22:00 至 2026-08-30 02:00",
);

// "Now" comes from the server's Date header, not the browser clock. The
// header below is a day and a business-day boundary away from any machine
// running this, so a browser clock leaking in shows up as a wrong answer.
const stamped = new Response(null, {
  headers: { date: new Date("2027-03-01T16:30:00Z").toUTCString() },
});
noteServerTime(stamped);
ok("記下了伺服器時鐘的差距", serverClockGapMs() !== 0);
eq("今天是伺服器的今天", businessToday(), "2027-03-02");
ok("時鐘走在伺服器時間上", businessClock().startsWith("00:30"));
eq("單據編號提示跟著伺服器年月", docNumberHint("V"), "例如 V2703-001");

// A response without the header leaves the known skew alone.
const gap = serverClockGapMs();
noteServerTime(new Response(null));
eq("沒有 Date 標頭就不動", serverClockGapMs(), gap);

console.log(failed === 0 ? "\n全部通過" : `\n${failed} 項未通過`);
process.exit(failed === 0 ? 0 : 1);
