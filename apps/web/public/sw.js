// The installed web app's offline shell (W-2096). The page opens with no
// connection, and the app's hashed chunks load from the phone, not the network.
//
// - A page load goes to the network first, so a deploy shows at once. With no
//   network, a server error or no answer in 3 s, the last app page saved here
//   answers instead. Only a page that names its build (index.html) is saved;
//   other pages under the scope, such as pipedream-connect.html, are not.
//   (Upstream removed its worker in macro-inc/macro#8148 because it served the
//   saved page first and left tabs on old builds; this one never does online.)
// - A hashed chunk never changes, so it is served from here first and saved
//   on first use. A new build (the page's macro-bundle-build) drops the old
//   chunks and saves the new page's startup files.
// - API calls and other origins pass through untouched.
//
// NOTE(shortcut): only startup files and chunks already used are saved, so a
// view never opened online can't open offline; precache its chunks when one
// matters offline.

const SHELL = 'aesv-shell';
const ASSETS = 'aesv-assets';
const SCOPE = new URL('./', self.location.href).pathname;
const HASHED = /-[A-Za-z0-9_-]{8}\.(?:js|css|wasm|woff2?|png|svg|webp)$/;
const SLOW_MS = 3000;

const buildOf = (html) =>
  html.match(/name="macro-bundle-build" content="([^"]*)"/)?.[1] ?? '';
const startupOf = (html) =>
  [...html.matchAll(/(?:src|href)="([^"]+\.(?:js|css))"/g)]
    .map((match) => match[1])
    .filter((path) => path.startsWith(SCOPE));

async function keepShell(response) {
  const html = await response.text();
  if (!buildOf(html)) return;
  const shell = await caches.open(SHELL);
  const previous = await shell.match(SCOPE);
  const previousBuild = previous ? buildOf(await previous.text()) : null;
  await shell.put(
    SCOPE,
    new Response(html, { headers: { 'content-type': 'text/html' } })
  );
  if (previousBuild === buildOf(html)) return;
  await caches.delete(ASSETS);
  await (await caches.open(ASSETS)).addAll(startupOf(html));
}

const savedShell = async () =>
  (await caches.match(SCOPE, { cacheName: SHELL })) ?? Response.error();

self.addEventListener('install', (event) => {
  event.waitUntil(
    fetch(SCOPE)
      .then((response) => (response.ok ? keepShell(response) : undefined))
      .catch(() => undefined)
  );
  self.skipWaiting();
});

self.addEventListener('activate', (event) =>
  event.waitUntil(self.clients.claim())
);

self.addEventListener('fetch', (event) => {
  const { request } = event;
  const url = new URL(request.url);
  if (request.method !== 'GET' || url.origin !== self.location.origin) return;
  if (!url.pathname.startsWith(SCOPE)) return;

  if (request.mode === 'navigate') {
    let saving = Promise.resolve();
    const network = fetch(request)
      .then((response) => {
        // A redirect (the owner gate's sign-in) is opaque and never saved.
        if (response.status >= 500) return savedShell();
        if (
          response.ok &&
          response.headers.get('content-type')?.includes('text/html')
        ) {
          saving = keepShell(response.clone());
        }
        return response;
      })
      .catch(savedShell);
    // The page saves after it answers, even when the saved shell answered first.
    event.waitUntil(network.then(() => saving));
    const slow = new Promise((resolve) => setTimeout(resolve, SLOW_MS)).then(
      async () => (await caches.match(SCOPE, { cacheName: SHELL })) ?? network
    );
    event.respondWith(Promise.race([network, slow]));
    return;
  }

  if (!HASHED.test(url.pathname)) return;
  event.respondWith(
    caches.match(request, { cacheName: ASSETS }).then(
      (saved) =>
        saved ??
        fetch(request).then((response) => {
          if (response.ok) {
            const copy = response.clone();
            event.waitUntil(
              caches.open(ASSETS).then((assets) => assets.put(request, copy))
            );
          }
          return response;
        })
    )
  );
});
