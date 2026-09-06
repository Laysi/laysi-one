// Prints the demo sign-ins on the demo site's login screen.
//
// A Cloudflare Worker on the route "demo.laysi.one/*". Snippets are the
// lighter tool for this and the zone is not entitled to them. It lives at the
// edge rather than in web/src so the product never learns the word "demo":
// one shared evaluation install is a deployment arrangement, and a banner for
// it is not a feature customers should carry, gate or configure.
//
// Deployed with the Cloudflare API; see deploy/README.md. This file is the
// source of truth for what is deployed -- edit here, then upload.
//
// The panel shows only while a password field is on the page, which is the
// login screen and nothing else. The session cookie is HttpOnly, so asking
// the markup is the only way to know, and the answer survives a redesign
// better than a class name would.
//
// No template literals anywhere in this file, deliberately: it is uploaded by
// pasting the source into another script, and a backtick there needs escaping
// that is easy to get wrong. The first upload went through base64 and atob(),
// which turned every UTF-8 byte into its own character and served the panel
// double-encoded.

const PANEL = [
  '<aside id="laysi-demo-hint" hidden style="',
  'position:fixed;left:16px;bottom:16px;z-index:2147483000;max-width:320px;',
  "border:1px solid #c9c4b8;background:#fbfaf7;color:#2b2b2b;",
  "font:13px/1.7 system-ui,-apple-system,'Noto Sans TC',sans-serif;",
  "padding:14px 16px;border-radius:2px;box-shadow:0 1px 4px rgba(0,0,0,.08);",
  // The panel is an overlay on somebody else's page. Without this it swallows
  // clicks meant for whatever sits beneath it, and on a short window that is
  // the login button. The table takes pointer events back, so the password is
  // still selectable.
  'pointer-events:none">',
  '<div style="font-weight:600;letter-spacing:.04em;margin-bottom:8px">示範帳號</div>',
  '<table style="border-collapse:collapse;width:100%;pointer-events:auto">',
  row("負責人", "boss"),
  row("人資", "hr"),
  row("會計", "acct"),
  row("職員", "demo"),
  row("系統管理員", "admin"),
  '<tr><td style="padding:7px 12px 1px 0;color:#6b6b6b">密碼</td>',
  '<td style="padding-top:7px"><code>laysi-demo-2026</code></td></tr>',
  "</table>",
  '<p style="margin:10px 0 0;color:#6b6b6b">本站每日凌晨四點整份重建，畫面上的都是示範資料。</p>',
  "</aside>",
  "<script>",
  "(function () {",
  "  var panel = document.getElementById('laysi-demo-hint');",
  "  if (!panel) return;",
  "  var queued = false;",
  "  function sync() {",
  "    queued = false;",
  "    panel.hidden = !document.querySelector('input[type=password]');",
  "  }",
  // The app renders after this runs and re-renders on every navigation, so
  // the check repeats -- coalesced to one per frame, because an SPA mutates
  // the tree far more often than it changes screens.
  "  function schedule() {",
  "    if (queued) return;",
  "    queued = true;",
  "    requestAnimationFrame(sync);",
  "  }",
  "  schedule();",
  "  new MutationObserver(schedule).observe(document.documentElement,",
  "    { childList: true, subtree: true });",
  "})();",
  "<\/script>",
].join("\n");

function row(label, name) {
  return (
    '<tr><td style="padding:1px 12px 1px 0;color:#6b6b6b">' +
    label +
    "</td><td><code>" +
    name +
    "</code></td></tr>"
  );
}

export default {
  async fetch(request) {
    const response = await fetch(request);
    // Only the document. Injecting into an API response would corrupt it, and
    // the app fetches JSON from this same origin.
    if (!(response.headers.get("content-type") || "").includes("text/html")) {
      return response;
    }
    return new HTMLRewriter()
      .on("body", {
        element(body) {
          body.append(PANEL, { html: true });
        },
      })
      .transform(response);
  },
};
