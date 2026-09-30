#!/usr/bin/env bun
// Routing and the sidebar, checked without a browser.
//
// The URL is the page state: SCREEN_PATH is the only map between a ScreenId
// and a path, and App.tsx reads it in both directions. Two screens sharing a
// path would collapse silently, because the reverse map is built with
// Object.fromEntries. visibleEntries is what decides whether an entry is
// drawn at all, so a heading left with nothing under it is its bug to catch.
//
//   bun scripts/nav-test.ts

import {
  FINANCE_NAV,
  HR_NAV,
  ME_NAV,
  SCREEN_PATH,
  SYSTEM_NAV,
  UPLOAD_PAGE_PATH,
  firstScreen,
  screenForPath,
  screenOp,
  screenTitle,
  uploadLinkSecret,
  visibleEntries,
  type NavItem,
  type ScreenId,
} from "../web/src/nav";

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

const ids = Object.keys(SCREEN_PATH) as ScreenId[];

eq("每個畫面都有自己的網址", new Set(Object.values(SCREEN_PATH)).size, ids.length);

const roundTrip = ids.filter((id) => screenForPath(SCREEN_PATH[id]) !== id);
eq(`網址對得回畫面（${ids.length} 個）`, roundTrip.join("、"), "");

// A screen with no entry in any of the four trees cannot be opened and its
// permission cannot gate anything: nothing draws it.
const orphans = ids.filter((id) => screenOp(id) === null);
eq("每個畫面都有導覽入口", orphans.join("、"), "");

const items = [...FINANCE_NAV, ...HR_NAV, ...ME_NAV, ...SYSTEM_NAV].filter(
  (e): e is NavItem => e.kind === "item",
);
eq("導覽項目數等於畫面數", items.length, ids.length);
eq("沒有兩個項目指向同一個畫面", new Set(items.map((i) => i.id)).size, items.length);

eq("尾端斜線視為同一個網址", screenForPath("/finance/ledger/"), "gl");
eq("沒有對應的網址回 null", screenForPath("/finance/nowhere"), null);
eq("根路徑不是任何畫面", screenForPath("/"), null);

eq("畫面標題取自導覽標籤", screenTitle("gl"), "傳票 · 總帳");
eq("認不得的畫面退回首頁", screenTitle("nowhere" as ScreenId), "首頁");
eq("畫面對應的操作", screenOp("mepay"), "list_my_payslips");

// Deny by default: no permission, no entry, and nothing to land on.
eq("沒有權限就沒有任何項目", visibleEntries(ME_NAV, []).length, 0);
eq("沒有權限就沒有落地畫面", firstScreen(ME_NAV, []), null);
eq("落地畫面是第一個叫得動的", firstScreen(ME_NAV, ["get_my_home"]), "me");
eq("落地畫面依導覽順序而非權限順序", firstScreen(ME_NAV, ["list_my_payslips", "punch_clock"]), "meclock");

// A heading with nothing under it is a label for an empty space.
const onlyMcp = visibleEntries(ME_NAV, ["list_my_api_tokens"]);
eq("只剩一項時只留它的分段標題", onlyMcp.length, 2);
eq("留下的標題是它自己那一段", onlyMcp[0]?.kind === "head" ? onlyMcp[0].label : "", "連線");
ok(
  "沒有一個標題後面是空的",
  visibleEntries(ME_NAV, ["punch_clock", "list_my_payslips"]).every(
    (e, i, all) => e.kind !== "head" || all[i + 1]?.kind === "item",
  ),
);

// The four trees are separate: a finance permission opens no HR screen.
eq("財務權限不會打開人資畫面", firstScreen(HR_NAV, ["get_dashboard"]), null);
eq("系統分頁同樣要自己的權限", firstScreen(SYSTEM_NAV, ["get_dashboard"]), null);

// The upload page sits outside every screen: it is opened by whoever holds
// the link, signed in or not, and no screen path may shadow it.
const secret = `lysu_${"a1".repeat(32)}`;
eq("上傳頁的網址取得出連結", uploadLinkSecret(`${UPLOAD_PAGE_PATH}${secret}`), secret);
eq("上傳頁尾端斜線視為同一個網址", uploadLinkSecret(`${UPLOAD_PAGE_PATH}${secret}/`), secret);
eq("格式不對的連結不當成上傳頁", uploadLinkSecret(`${UPLOAD_PAGE_PATH}lysu_short`), null);
eq("其他網址不是上傳頁", uploadLinkSecret("/finance/documents"), null);
ok(
  "沒有畫面的網址落在上傳頁底下",
  ids.every((id) => !SCREEN_PATH[id].startsWith(UPLOAD_PAGE_PATH)),
);

console.log(failed === 0 ? "\n全部通過" : `\n${failed} 項未通過`);
process.exit(failed === 0 ? 0 : 1);
