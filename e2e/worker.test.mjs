// End-to-end tests: build the Worker, run it under `wrangler dev` with a
// fresh local D1, and exercise it over HTTP. Run with `npm test`, or just
// these with `npm run test:e2e`. The first run compiles the Worker, which
// can take a few minutes.

import assert from "node:assert/strict";
import { execFileSync, spawn, spawnSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { after, before, test } from "node:test";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const wranglerBin = join(root, "node_modules", "wrangler", "bin", "wrangler.js");
const state = mkdtempSync(join(tmpdir(), "viewtally-e2e-"));

const ORIGIN = "http://localhost:8080";
const SITES = {
  demo: [ORIGIN, "https://*.example.test"],
  blog: ["https://blog.test"],
  stats: [ORIGIN],
  cache: [ORIGIN],
  limits: [ORIGIN],
};
const BROWSER = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0";
const CRON = "5 0 * * *";

let base;
let dev;
let devOutput = "";

function wrangler(...args) {
  return execFileSync(process.execPath, [wranglerBin, ...args], {
    cwd: root,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
  });
}

function sql(command) {
  const out = wrangler("d1", "execute", "viewtally", "--local", "--persist-to", state, "--json", "--command", command);
  return JSON.parse(out).at(-1).results;
}

function freePort() {
  return new Promise((resolve, reject) => {
    const server = createServer();
    server.once("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      server.close(() => resolve(port));
    });
  });
}

before(
  async () => {
    wrangler("d1", "migrations", "apply", "viewtally", "--local", "--persist-to", state);
    const port = await freePort();
    base = `http://127.0.0.1:${port}`;
    dev = spawn(
      process.execPath,
      [
        wranglerBin, "dev",
        "--ip", "127.0.0.1",
        "--port", String(port),
        "--persist-to", state,
        "--test-scheduled",
        "--var", `SITES:${JSON.stringify(SITES)}`,
        "--var", "CACHE_SECONDS:60",
      ],
      { cwd: root, stdio: ["ignore", "pipe", "pipe"] },
    );
    dev.stdout.on("data", (chunk) => (devOutput += chunk));
    dev.stderr.on("data", (chunk) => (devOutput += chunk));

    const deadline = Date.now() + 9 * 60_000;
    while (Date.now() < deadline) {
      if (dev.exitCode !== null) break;
      try {
        if ((await fetch(`${base}/`)).ok) return;
      } catch {
        // Not listening yet.
      }
      await new Promise((resolve) => setTimeout(resolve, 1_000));
    }
    throw new Error(`wrangler dev did not start:\n${devOutput}`);
  },
  { timeout: 10 * 60_000 },
);

after(() => {
  if (dev?.pid) {
    // wrangler starts workerd as a child process, so stop the whole tree.
    if (process.platform === "win32") {
      spawnSync("taskkill", ["/pid", String(dev.pid), "/t", "/f"]);
    } else {
      dev.kill("SIGINT");
    }
  }
  try {
    rmSync(state, { recursive: true, force: true, maxRetries: 5, retryDelay: 500 });
  } catch {
    // Windows can hold the SQLite files a moment longer; it's a temp dir.
  }
});

function hit(body, { origin = ORIGIN, ua = BROWSER, ip = "203.0.113.1", headers = {}, query = "" } = {}) {
  return fetch(`${base}/hit${query}`, {
    method: "POST",
    headers: { ...(origin && { Origin: origin }), "User-Agent": ua, "CF-Connecting-IP": ip, ...headers },
    body: typeof body === "string" ? body : JSON.stringify(body),
  });
}

async function hitJson(body, options) {
  const res = await hit(body, options);
  assert.equal(res.status, 200, await res.clone().text());
  return res.json();
}

// A unique query parameter gives every read its own edge-cache key, so
// tests see fresh numbers. The cache test reads without it.
let bust = 0;
function get(path, headers = {}) {
  const sep = path.includes("?") ? "&" : "?";
  return fetch(`${base}${path}${sep}_=${++bust}`, { headers });
}

async function getJson(path) {
  const res = await get(path);
  assert.equal(res.status, 200, await res.clone().text());
  return res.json();
}

function isoDay(offset = 0) {
  return new Date(Date.now() + offset * 86_400_000).toISOString().slice(0, 10);
}

test("root describes the Worker", async () => {
  const about = await getJson("/");
  assert.equal(about.name, "viewtally");
  assert.match(about.version, /^\d+\.\d+\.\d+$/);
});

test("counts a first view and skips repeats the same day", async () => {
  const page = { site: "demo", path: "/dedupe" };
  assert.deepEqual(await hitJson(page), { counted: true });
  assert.deepEqual(await hitJson({ site: "demo", path: "/dedupe/?ref=x#top" }), { counted: false, reason: "duplicate" });
  assert.deepEqual(await hitJson(page, { ip: "203.0.113.2" }), { counted: true });
  assert.deepEqual(await hitJson(page, { ua: `${BROWSER} Edg/140.0` }), { counted: true });
  assert.deepEqual(await hitJson({ site: "demo", path: "/dedupe/other" }), { counted: true });

  const views = await getJson("/views?site=demo&path=/dedupe");
  assert.equal(views.views, 3);
});

test("skips bots and prefetches", async () => {
  const page = { site: "demo", path: "/bots" };
  assert.deepEqual(await hitJson(page, { ua: "Mozilla/5.0 (compatible; Googlebot/2.1)" }), { counted: false, reason: "bot" });
  assert.deepEqual(await hitJson(page, { ua: "" }), { counted: false, reason: "bot" });
  assert.deepEqual(await hitJson(page, { headers: { "Sec-Purpose": "prefetch;prerender" } }), { counted: false, reason: "prefetch" });
  assert.equal((await getJson("/views?site=demo&path=/bots")).views, 0);
});

test("only a site's own origins may count its views", async () => {
  const page = { site: "demo", path: "/origins" };
  // `null` sends no Origin header at all.
  for (const origin of ["https://evil.test", "https://blog.test", "null", null]) {
    const res = await hit(page, { origin });
    assert.equal(res.status, 403, `origin ${origin}`);
  }
  assert.deepEqual(await hitJson(page, { origin: "https://www.example.test" }), { counted: true });
  assert.deepEqual(await hitJson({ site: "blog", path: "/post" }, { origin: "https://blog.test" }), { counted: true });
});

test("rejects bad hits", async () => {
  assert.equal((await hit({ site: "nope", path: "/" })).status, 404);
  assert.equal((await hit({ site: "demo" })).status, 400);
  assert.equal((await hit({ path: "/" })).status, 400);
  assert.equal((await hit({ site: "demo", path: "about" })).status, 400);
  assert.equal((await hit({ site: "demo", path: `/${"a".repeat(300)}` })).status, 400);
  assert.equal((await hit("site=demo&path=/")).status, 400);
  assert.equal((await hit({ site: "demo", path: "/", pad: "x".repeat(3000) })).status, 413);
});

test("accepts site and path as query parameters", async () => {
  const res = await hit("", { origin: "https://blog.test", ip: "203.0.113.9", query: "?site=blog&path=/from-query" });
  assert.deepEqual(await res.json(), { counted: true });
});

test("sends CORS headers only to configured origins", async () => {
  const preflight = (origin) =>
    fetch(`${base}/hit`, {
      method: "OPTIONS",
      headers: { Origin: origin, "Access-Control-Request-Method": "POST", "Access-Control-Request-Headers": "content-type" },
    });

  const allowed = await preflight(ORIGIN);
  assert.equal(allowed.status, 204);
  assert.equal(allowed.headers.get("access-control-allow-origin"), ORIGIN);
  assert.match(allowed.headers.get("access-control-allow-methods"), /POST/);
  assert.match(allowed.headers.get("access-control-allow-headers"), /Content-Type/i);

  const denied = await preflight("https://evil.test");
  assert.equal(denied.status, 403);
  assert.equal(denied.headers.get("access-control-allow-origin"), null);

  const read = await get("/views?site=demo", { Origin: "https://blog.test" });
  assert.equal(read.headers.get("access-control-allow-origin"), "https://blog.test");
  assert.equal(read.headers.get("vary"), "Origin");

  const foreign = await get("/views?site=demo", { Origin: "https://evil.test" });
  assert.equal(foreign.status, 200);
  assert.equal(foreign.headers.get("access-control-allow-origin"), null);
});

test("reports all-time, rolling-window and per-page totals", async () => {
  for (const [path, ip] of [["/", "10.0.0.1"], ["/a", "10.0.0.1"], ["/a", "10.0.0.2"], ["/b/", "10.0.0.3"]]) {
    assert.deepEqual(await hitJson({ site: "stats", path }, { ip }), { counted: true });
  }
  const today = isoDay();

  assert.deepEqual(await getJson("/views?site=stats"), { site: "stats", path: null, days: null, from: null, to: null, views: 4 });
  assert.deepEqual(await getJson("/views?site=stats&days=365"), {
    site: "stats",
    path: null,
    days: 365,
    from: isoDay(-364),
    to: today,
    views: 4,
  });
  assert.equal((await getJson("/views?site=stats&days=1")).views, 4);
  assert.equal((await getJson("/views?site=stats&path=/a")).views, 2);
  assert.equal((await getJson("/views?site=stats&path=/b")).views, 1);
  assert.equal((await getJson("/views?site=stats&path=/never")).views, 0);

  assert.deepEqual((await getJson("/pages?site=stats&days=30")).pages, [
    { path: "/a", views: 2 },
    { path: "/", views: 1 },
    { path: "/b", views: 1 },
  ]);
  assert.deepEqual((await getJson("/pages?site=stats&limit=1")).pages, [{ path: "/a", views: 2 }]);

  const daily = await getJson("/daily?site=stats&days=3");
  assert.deepEqual(daily.series, [
    { date: isoDay(-2), views: 0 },
    { date: isoDay(-1), views: 0 },
    { date: today, views: 4 },
  ]);
  assert.equal((await getJson("/daily?site=stats")).series.length, 30);
  assert.deepEqual((await getJson("/daily?site=stats&path=/a&days=1")).series, [{ date: today, views: 2 }]);
});

test("serves the count as an SVG badge", async () => {
  const res = await get("/badge.svg?site=stats");
  assert.equal(res.status, 200);
  assert.match(res.headers.get("content-type"), /^image\/svg\+xml/);
  const svg = await res.text();
  assert.match(svg, /^<svg /);
  assert.match(svg, /aria-label="views: 4"/);

  const custom = await (await get("/badge.svg?site=stats&path=/a&days=365&label=page%20views&color=e05d44")).text();
  assert.match(custom, /aria-label="page views: 2"/);
  assert.match(custom, /fill="#e05d44"/);
  assert.equal((await get("/badge.svg?site=stats&color=red")).status, 400);
});

test("validates query parameters and methods", async () => {
  for (const path of ["/views", "/views?site=stats&days=0", "/views?site=stats&days=year", "/pages?site=stats&limit=0", "/daily?site=stats&path=x"]) {
    assert.equal((await get(path)).status, 400, path);
  }
  assert.equal((await get("/views?site=nope")).status, 404);
  assert.equal((await get("/nope")).status, 404);

  const wrongMethod = await fetch(`${base}/views?site=stats`, { method: "DELETE" });
  assert.equal(wrongMethod.status, 405);
  assert.equal(wrongMethod.headers.get("allow"), "GET, OPTIONS");
  assert.equal((await fetch(`${base}/hit`)).status, 405);
});

test("caches stats responses at the edge", async () => {
  const url = `${base}/views?site=cache`;
  const first = await fetch(url);
  assert.equal(first.headers.get("cache-control"), "public, max-age=60");
  assert.equal((await first.json()).views, 0);

  assert.deepEqual(await hitJson({ site: "cache", path: "/" }), { counted: true });
  assert.equal((await (await fetch(url)).json()).views, 0, "the cached count is served");
  assert.equal((await getJson("/views?site=cache")).views, 1, "a new URL reads D1");
});

test("rate limits hits from one IP", async () => {
  const statuses = [];
  for (let i = 0; i < 12; i++) {
    statuses.push((await hit({ site: "limits", path: `/${i}` }, { ip: "198.51.100.77" })).status);
  }
  assert.deepEqual(statuses, [...Array(10).fill(200), 429, 429]);
  assert.equal((await hit({ site: "limits", path: "/other-ip" }, { ip: "198.51.100.78" })).status, 200);
});

test("the daily cron deletes earlier salts and visitor hashes", async () => {
  sql("INSERT INTO salts (day, salt) VALUES (1, 'old'); INSERT INTO visitors (day, hash) VALUES (1, 'a'), (1, 'b');");
  const [before] = sql("SELECT COUNT(*) AS old FROM visitors WHERE day = 1");
  assert.equal(before.old, 2);

  const res = await fetch(`${base}/__scheduled?cron=${encodeURIComponent(CRON)}`);
  assert.equal(res.status, 200);

  const [after] = sql(
    "SELECT (SELECT COUNT(*) FROM visitors WHERE day = 1) AS old_visitors, " +
      "(SELECT COUNT(*) FROM salts WHERE day = 1) AS old_salts, " +
      "(SELECT COUNT(*) FROM visitors) AS today_visitors, " +
      "(SELECT COUNT(*) FROM salts) AS today_salts",
  );
  assert.equal(after.old_visitors, 0);
  assert.equal(after.old_salts, 0);
  assert.ok(after.today_visitors > 0, "today's hashes are kept");
  assert.equal(after.today_salts, 1);

  // Today's dedupe still works after the purge.
  assert.deepEqual(await hitJson({ site: "demo", path: "/dedupe" }), { counted: false, reason: "duplicate" });
});

test("stores no raw IP addresses or user agents", () => {
  const rows = sql(
    "SELECT hash AS value FROM visitors UNION ALL SELECT salt FROM salts " +
      "UNION ALL SELECT path FROM page_views UNION ALL SELECT site FROM site_views",
  );
  const stored = rows.map((row) => String(row.value)).join("\n");
  for (const secret of ["203.0.113.1", "10.0.0.1", "198.51.100.77", "Firefox", "Mozilla"]) {
    assert.ok(!stored.includes(secret), `${secret} must not be stored`);
  }
});
