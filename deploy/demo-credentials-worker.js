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

// Every value is a button that copies itself. Selecting the text by hand does
// not work here: a double click stops at the hyphen and takes "laysi" alone,
// and dragging across the row picks up the whitespace between the cells. A
// password that copies wrong is worse than no panel, because the screen then
// says the password is incorrect and the panel looks right.
const ROWS = [
  ["負責人", "boss"],
  ["人資", "hr"],
  ["會計", "acct"],
  ["職員", "demo"],
  ["系統管理員", "admin"],
  ["密碼", "laysi-demo-2026"],
];

const VALUE_STYLE = [
  "font:inherit;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;",
  "background:none;border:1px solid transparent;border-radius:2px;",
  "padding:1px 5px;margin:-1px -5px;color:inherit;cursor:copy;",
  "pointer-events:auto",
].join("");

const LABEL_STYLE = "padding:2px 14px 2px 0;color:#6b6b6b;white-space:nowrap";

// Joined with no separator: whitespace between tags is selectable text, and
// this panel exists to be copied from.
const MARKUP = [
  '<aside id="laysi-demo-hint" hidden style="',
  "position:fixed;left:16px;bottom:16px;z-index:2147483000;max-width:340px;",
  "border:1px solid #c9c4b8;background:#fbfaf7;color:#2b2b2b;",
  "font:13px/1.7 system-ui,-apple-system,'Noto Sans TC',sans-serif;",
  "padding:14px 16px;border-radius:2px;box-shadow:0 1px 4px rgba(0,0,0,.08);",
  // The panel is an overlay on somebody else's page. Without this it swallows
  // clicks meant for whatever sits beneath it, and on a short window that is
  // the login button. The buttons take pointer events back.
  'pointer-events:none">',
  '<div style="font-weight:600;letter-spacing:.04em;margin-bottom:6px">',
  "示範帳號</div>",
  '<table style="border-collapse:collapse">',
  ROWS.map(row).join(""),
  "</table>",
  '<p id="laysi-demo-note" style="margin:8px 0 0;color:#6b6b6b">',
  "點一下即複製。本站每日凌晨四點整份重建。</p>",
  "</aside>",
].join("");

function row(pair) {
  return (
    '<tr><td style="' +
    LABEL_STYLE +
    '">' +
    pair[0] +
    '</td><td><button type="button" data-copy="' +
    pair[1] +
    '" style="' +
    VALUE_STYLE +
    '">' +
    pair[1] +
    "</button></td></tr>"
  );
}

const SCRIPT = [
  "<script>",
  "(function () {",
  "  var panel = document.getElementById('laysi-demo-hint');",
  "  if (!panel) return;",
  "  var note = document.getElementById('laysi-demo-note');",
  "  var said = note.textContent;",
  "  var timer;",
  "  panel.addEventListener('click', function (event) {",
  "    var button = event.target.closest('button[data-copy]');",
  "    if (!button) return;",
  "    var text = button.getAttribute('data-copy');",
  "    var done = function () {",
  "      note.textContent = '已複製 ' + text;",
  "      button.style.borderColor = '#c9c4b8';",
  "      clearTimeout(timer);",
  "      timer = setTimeout(function () {",
  "        note.textContent = said;",
  "        button.style.borderColor = 'transparent';",
  "      }, 2000);",
  "    };",
  // The clipboard API needs a secure context and permission; when it is not
  // there, selecting the button's text still leaves a working ctrl-C.
  "    if (navigator.clipboard) {",
  "      navigator.clipboard.writeText(text).then(done, select);",
  "    } else {",
  "      select();",
  "    }",
  "    function select() {",
  "      var range = document.createRange();",
  "      range.selectNodeContents(button);",
  "      var selection = getSelection();",
  "      selection.removeAllRanges();",
  "      selection.addRange(range);",
  "      note.textContent = '已選取，請按 Ctrl-C';",
  "    }",
  "  });",
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

export default {
  async fetch(request) {
    const response = await fetch(request);
    // Only the document. Injecting into an API response would corrupt it, and
    // the app fetches JSON from this same origin.
    if (!(response.headers.get("content-type") || "").includes("text/html")) {
      return response;
    }
    const rewritten = new HTMLRewriter()
      .on("body", {
        element(body) {
          body.append(MARKUP + SCRIPT, { html: true });
        },
      })
      .transform(response);
    // Keep the demo out of search results. The only page a crawler reaches is
    // the sign-in screen, which carries the shared passwords this worker
    // prints; indexed, it also competes with laysi.one on the brand name. A
    // header rather than a meta tag: the markup is the product's and this
    // arrangement is not. Headers from fetch() are immutable, so the response
    // is rebuilt to set one.
    const out = new Response(rewritten.body, rewritten);
    out.headers.set("X-Robots-Tag", "noindex, nofollow");
    return out;
  },
};
