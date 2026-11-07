// Lumen service worker — makes the WebAssembly app installable (Android "Add to Home screen" / iOS "Share → Add to Home Screen")
// and usable offline. The build id is stamped in by build-web.sh, so every build gets its own cache and old ones are deleted.
const BUILD = "__BUILD__";
const CACHE = "lumen-" + BUILD;
const SHELL = [
  "/webapp/lumen.js?v=" + BUILD,
  "/webapp/lumen_bg.wasm?v=" + BUILD,
  "/webapp/manifest.webmanifest",
  "/webapp/icons/icon-192.png",
  "/webapp/icons/icon-512.png",
];
const isAppPage = (u) => u.pathname === "/app" || u.pathname === "/app/" || u.pathname === "/webapp" || u.pathname === "/webapp/" || u.pathname === "/webapp/index.html";

self.addEventListener("install", (e) => {
  e.waitUntil(caches.open(CACHE).then((c) => c.addAll(SHELL)).then(() => self.skipWaiting()).catch(() => self.skipWaiting()));
});

self.addEventListener("activate", (e) => {
  e.waitUntil(
    caches.keys().then((keys) => Promise.all(keys.filter((k) => k.startsWith("lumen-") && k !== CACHE).map((k) => caches.delete(k)))).then(() => self.clients.claim())
  );
});

async function networkFirst(req, key) {
  const cache = await caches.open(CACHE);
  try {
    const res = await fetch(req);
    if (res && res.ok) cache.put(key, res.clone());
    return res;
  } catch (err) {
    const hit = await cache.match(key);
    if (hit) return hit;
    throw err;
  }
}

self.addEventListener("fetch", (e) => {
  const req = e.request;
  if (req.method !== "GET") return;
  const url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  if (isAppPage(url)) {
    e.respondWith(networkFirst(req, "/__lumen_shell__"));
    return;
  }
  if (!url.pathname.startsWith("/webapp/") || url.pathname === "/webapp/sw.js") return;
  if (url.searchParams.has("v")) {
    // versioned files never change: cache first
    e.respondWith(caches.open(CACHE).then(async (c) => (await c.match(req)) || networkFirst(req, req)));
  } else {
    e.respondWith(networkFirst(req, req));
  }
});
