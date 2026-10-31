# viewtally

[![crates.io](https://img.shields.io/crates/v/viewtally.svg)](https://crates.io/crates/viewtally)
[![portfolio views this year](https://viewtally.nylla.workers.dev/badge.svg?site=portfolio&days=365&label=portfolio%20views%20this%20year)](https://nylla.vercel.app)

A privacy-friendly page-view counter for personal websites, written in Rust
for Cloudflare Workers, with counts stored in D1. You deploy it into your own
Cloudflare account, on the free plan, with a few `wrangler` commands. Your
site then gets a small JSON API and an SVG badge.

- **Cookie-free counting:** a view is counted once per visitor, per page, per
  UTC day.
- **No personal data stored:** no IP addresses, no user agents, no cookies, no
  referrers.
- **Per-page and per-site totals:** all time, or over the last N days (for
  example "page views this year").
- **Several sites per deployment:** each site has its own allowed origins.
- **Sized for the free plan:** reads and writes stay within the D1 and Workers
  free limits.

## How counting works

When a page loads, the snippet sends `POST /hit` with the site id and the path.
The Worker then:

1. Checks that the request's `Origin` is one of the site's configured origins.
2. Skips crawlers, headless browsers, link previewers and prerenders.
3. Hashes the site, path, IP address and user agent together with a random
   salt that is created fresh each UTC day. It keeps only the first 128 bits of
   the SHA-256 hash.
4. Stores the hash in today's `visitors` table. If the hash was already there,
   the view is a repeat and isn't counted. Otherwise it adds one view to the
   page's and the site's count for the day.

A cron trigger runs at 00:05 UTC every day. It deletes the earlier days' salts
and visitor hashes. Once a salt is gone, nobody can link a hash back to a
visitor, or link one day's visitors to the next day's. The only data kept long
term is view counts per page per day.

The snippet also remembers which pages it has already reported today, in
`localStorage`, so repeat visits don't spend a request at all.

## Free plan budget

Checked against Cloudflare's docs on 2026-11-01. The free limits reset at
00:00 UTC.

| Limit (Workers Free) | Allowance | What viewtally uses |
| --- | --- | --- |
| Worker requests | 100,000 / day | 1 per counted view, plus your count reads |
| Worker CPU time | 10 ms / request | well under 1 ms per request (one SHA-256 and some JSON) |
| D1 rows written | 100,000 / day | about 4 per counted view, 0 per repeat |
| D1 rows read | 5,000,000 / day | at most `days` rows per total, cached for 60 s |
| D1 storage | 500 MB per database, 5 GB per account | one small row per page per day |
| Cron triggers | 5 per account | 1 |

The 4 writes per counted view are: the visitor hash, the page's daily count,
the site's daily count, and deleting the hash the next day. That works out to
roughly **25,000 counted views a day** on the free plan, which is plenty for a
personal site. If D1 runs out of writes, hits fail until 00:00 UTC, but reads
keep working.

## Set up

You need [Rust](https://rustup.rs) with the `wasm32-unknown-unknown` target,
and Node.js 22 or later.

```bash
rustup target add wasm32-unknown-unknown
git clone https://github.com/nylla8444/viewtally-rust.git
cd viewtally-rust
npm install
```

### Try it locally

No Cloudflare account is needed for this. `wrangler dev` simulates the Worker
and D1 on your machine.

```bash
npm run migrate:local
npm run dev
```

The first build compiles the Worker and takes a few minutes. Then try it from
another terminal. The `CF-Connecting-IP` header stands in for the visitor's
address.

```bash
curl -X POST http://localhost:8787/hit -H "Origin: http://localhost:8080" -H "User-Agent: Mozilla/5.0" -H "CF-Connecting-IP: 203.0.113.1" -d '{"site":"example","path":"/"}'
```

```bash
curl "http://localhost:8787/views?site=example&days=365"
```

### Deploy to Cloudflare

You need a free Cloudflare account. `wrangler login` opens your browser to
sign in.

If you've never deployed a Worker before, first pick your account's
workers.dev subdomain in the dashboard: go to **Workers & Pages**, then
**Settings**, then **Subdomain**. Every Worker you deploy is served at
`<worker-name>.<subdomain>.workers.dev`. If you skip this step, a
non-interactive `wrangler deploy` registers the Worker's own name as your
subdomain.

```bash
npx wrangler login
```

```bash
npx wrangler d1 create viewtally
```

Copy the `database_id` this prints into `wrangler.toml`. Next, set `SITES` in
`wrangler.toml` to your own sites (see [Configuration](#configuration)). Then
create the tables and deploy:

```bash
npm run migrate:remote
```

```bash
npm run deploy
```

Wrangler prints the Worker's URL, for example
`https://viewtally.<your-subdomain>.workers.dev`. You can also attach a
[custom domain](https://developers.cloudflare.com/workers/configuration/routing/custom-domains/).
After a schema change, run `npm run migrate:remote` again before deploying.

## Configuration

Everything is set in `wrangler.toml`.

```toml
[vars]
SITES = { portfolio = ["https://example.com", "https://*.example.com", "http://localhost:8080"], blog = ["https://blog.example.com"] }
CACHE_SECONDS = "60"
```

- **`SITES`** maps each site id to the browser origins allowed to count its
  views.
  - An origin is a scheme and host with no path, like
    `https://example.com`.
  - One `*` may stand for a subdomain, which is handy for preview deployments.
    Remember that `https://*.vercel.app` would let any Vercel site count views
    for you.
  - Browsers may read the stats from any origin listed under any site.
  - `.dev.vars` and `wrangler dev --var` can also give `SITES` as a JSON
    string.
  - To read your counts while you develop your site locally, without counting
    those visits, add a site that only lists your dev server, like
    `dev = ["http://localhost:8080"]`. Reads from localhost are then allowed,
    while hits from localhost for your real site are refused.
- **`CACHE_SECONDS`** sets how long stats responses are cached in browsers and,
  where the Cache API is available, at Cloudflare's edge. `"0"` turns caching
  off.
- **`[[ratelimits]] HIT_LIMITER`** allows 10 hits a minute per visitor IP, per
  Cloudflare location. The limit is checked against a salted hash of the IP,
  never the IP itself. If your account rejects this binding when you deploy,
  delete the block: the Worker skips the check when the binding is missing.
- **`[triggers] crons`** sets when the daily purge runs.

If you deploy from a public fork, you can keep your database id and origins
out of git. Copy `wrangler.toml` to `wrangler.prod.toml`, which is already
gitignored, and pass `-c wrangler.prod.toml` to the `wrangler d1 migrations`
and `wrangler deploy` commands.

## Add it to your site

Paste this before `</body>`, with your Worker's URL and site id:

```html
<script>
  (function () {
    var endpoint = "https://viewtally.YOUR-SUBDOMAIN.workers.dev";
    var site = "portfolio";

    function track() {
      if (navigator.webdriver) return;
      var path = location.pathname;
      var today = new Date().toISOString().slice(0, 10); // UTC, like the Worker
      var key = "viewtally:" + site + ":" + path;
      try {
        if (localStorage.getItem(key) === today) return;
        localStorage.setItem(key, today);
      } catch (e) {}
      fetch(endpoint + "/hit", {
        method: "POST",
        body: JSON.stringify({ site: site, path: path }),
        keepalive: true,
      }).catch(function () {});
    }

    if (document.prerendering) {
      document.addEventListener("prerenderingchange", track, { once: true });
    } else {
      track();
    }
    // Single-page apps: call viewtally() after each client-side navigation.
    window.viewtally = track;
  })();
</script>
```

The body is sent without a `Content-Type` header. That makes it a "simple"
cross-origin request, so the browser sends no extra preflight request.

To show a count on the page:

```html
<span id="views">–</span> views this year
<script>
  fetch("https://viewtally.YOUR-SUBDOMAIN.workers.dev/views?site=portfolio&days=365")
    .then(function (r) { return r.json(); })
    .then(function (d) { document.getElementById("views").textContent = d.views.toLocaleString(); });
</script>
```

Or embed a badge, which works anywhere an image does, including GitHub
READMEs:

```markdown
![views](https://viewtally.YOUR-SUBDOMAIN.workers.dev/badge.svg?site=portfolio&days=365)
```

## API

Every route takes `site`. The other parameters are optional. Paths are cleaned
up the same way everywhere:

- The query string and fragment are dropped, so `/blog?page=2#top` counts as
  `/blog`.
- Repeated slashes are merged, and a trailing slash is removed, so `/about/`
  counts as `/about`.

Days are UTC dates. `days=N` means today and the N−1 days before it, up to
3660 days. Leaving `days` out means all time.

### `POST /hit`

Counts a view. Send the body `{"site": "portfolio", "path": "/about"}`, or the
same two fields as query parameters. The request must carry an allowed
`Origin`.

```json
{"counted": true}
{"counted": false, "reason": "duplicate"}
```

The other reasons a view isn't counted are `bot` and `prefetch`. Error
responses are JSON like `{"error": "..."}`:

| Status | When |
| --- | --- |
| 400 | Bad input |
| 403 | The origin isn't allowed for the site |
| 404 | Unknown site |
| 413 | The body is over 2 KB |
| 429 | Rate limited |

### `GET /views?site=S[&path=P][&days=N]`

Total views of a site, or of one page.

```json
{"site": "portfolio", "path": null, "days": 365, "from": "2025-11-02", "to": "2026-11-01", "views": 1234}
```

### `GET /pages?site=S[&days=N][&limit=L]`

The most viewed pages, most views first. `limit` defaults to 10 and goes up to
100.

```json
{"site": "portfolio", "days": 30, "from": "2026-10-03", "to": "2026-11-01", "pages": [{"path": "/", "views": 80}, {"path": "/projects", "views": 31}]}
```

### `GET /daily?site=S[&path=P][&days=N]`

Views per day, oldest first, with days that had no views included as 0.
`days` defaults to 30.

```json
{"site": "portfolio", "path": null, "days": 3, "from": "2026-10-30", "to": "2026-11-01", "series": [{"date": "2026-10-30", "views": 4}, {"date": "2026-10-31", "views": 0}, {"date": "2026-11-01", "views": 9}]}
```

### `GET /badge.svg?site=S[&path=P][&days=N][&label=L][&color=C]`

An SVG badge. The count is shortened like `1.2k`, always rounding down. `label`
defaults to `views`. `color` is a hex color like `4c1`, and defaults to
`007ec6`.

## What it doesn't do

- **No security against scripted spam.** A script can fake `Origin`,
  `User-Agent` and IP-like headers. The origin check stops other websites
  from counting views for you, and the rate limit slows down floods. A
  determined attacker can still inflate counts or use up the day's free D1
  writes.
- **No referrers, countries, devices or sessions.** If you need those, a full
  analytics tool such as Umami or GoatCounter is a better fit.

## Development

```bash
npm test
```

This runs two test suites:

- **Unit tests** (`cargo test`) cover dates and rolling windows, path cleanup,
  origin matching, bot detection, hashing, and the badge.
- **End-to-end tests** (`e2e/`) start the Worker under `wrangler dev` with a
  fresh local D1. They cover counting and dedupe, bots, origins and CORS, the
  totals, caching, the rate limit, the daily purge, and a check that no IPs or
  user agents end up in the database.

## License

[MIT](LICENSE)
