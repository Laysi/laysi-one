// App shell cache. The frontend ships no data offline: /v1, /mcp and /healthz
// always go to the network, so an offline launch reaches the interface and the
// interface reports that the server is unreachable.
const CACHE = 'laysi-shell-v1';
const SHELL = '/';

self.addEventListener('install', (event) => {
  event.waitUntil(
    caches
      .open(CACHE)
      .then((cache) => cache.add(SHELL))
      .then(() => self.skipWaiting()),
  );
});

self.addEventListener('activate', (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((key) => key !== CACHE).map((key) => caches.delete(key))))
      .then(() => self.clients.claim()),
  );
});

const put = (request, response) => caches.open(CACHE).then((cache) => cache.put(request, response));

self.addEventListener('fetch', (event) => {
  const request = event.request;
  if (request.method !== 'GET') return;

  const url = new URL(request.url);
  if (url.origin !== self.location.origin) return;
  if (/^\/(v1|mcp|healthz)\b/.test(url.pathname)) return;

  // Navigations are network-first: index.html is served with no-cache so a
  // deploy must be able to replace the bundle reference on the next load.
  if (request.mode === 'navigate') {
    event.respondWith(
      (async () => {
        try {
          const response = await fetch(request);
          event.waitUntil(put(SHELL, response.clone()));
          return response;
        } catch {
          const hit = await caches.match(SHELL);
          if (hit) return hit;
          throw new Error('offline');
        }
      })(),
    );
    return;
  }

  // Vite emits content-hashed filenames under /assets, so a cached entry can
  // never be stale: a changed file is a changed URL.
  if (url.pathname.startsWith('/assets/')) {
    event.respondWith(
      (async () => {
        const hit = await caches.match(request);
        if (hit) return hit;
        const response = await fetch(request);
        if (response.ok) event.waitUntil(put(request, response.clone()));
        return response;
      })(),
    );
  }
});
