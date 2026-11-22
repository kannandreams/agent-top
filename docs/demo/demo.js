// Loaded before app.js in the hosted demo (docs/demo/). The demo has no
// server, so every /api/ request is answered from a JSON file that
// docs/ui-demo.py exported from the invented store. Written by hand; the
// export writes everything else in this folder.
(() => {
  // With no session in the address, open the one named by data-open, so the
  // demo starts on a run graph. app.js reads the hash once the page loads.
  const open = document.currentScript && document.currentScript.dataset.open;
  if (open && !location.hash) history.replaceState(null, "", open);
  const DAY = 86_400_000;
  const key = (s) => String(s || "").replace(/[^A-Za-z0-9._-]/g, "_") || "_";
  const real = window.fetch.bind(window);
  window.fetch = (input, init) => {
    const url = new URL(String(input), location.href);
    const m = url.pathname.match(/\/api\/([a-z]+)$/);
    if (!m) return real(input, init);
    const p = url.searchParams;
    let file;
    switch (m[1]) {
      case "meta":
        file = "meta";
        break;
      case "overview":
      case "tools":
      case "mcp":
      case "sessions": {
        const since = Number(p.get("since") || 0);
        const range = since ? Math.round((Date.now() - since) / DAY) : 0;
        file = `${m[1]}-${range}-${key(p.get("harness"))}`;
        break;
      }
      case "session":
        file = `session/${key(p.get("harness"))}/${key(p.get("id"))}`;
        break;
      case "context":
        file = `context/${key(p.get("harness"))}/${key(p.get("id"))}`;
        break;
      case "spans":
        file = `spans/${key(p.get("harness"))}/${key(p.get("id"))}/${key(p.get("agent"))}`;
        break;
      default:
        return Promise.resolve(new Response("not found", { status: 404 }));
    }
    return real(new URL(`api/${file}.json`, document.baseURI), init);
  };
})();
