//! viewtally: a privacy-friendly page-view counter for personal websites,
//! running on Cloudflare Workers with D1 for storage.
//!
//! Routes:
//! - `POST /hit` counts a view of a page, once per visitor per page per UTC day.
//! - `GET /views` returns a site's or a page's views, all time or over the last N days.
//! - `GET /pages` returns a site's most viewed pages.
//! - `GET /daily` returns views per day.
//! - `GET /badge.svg` returns a view count as an SVG badge.

mod badge;
mod config;
mod day;
mod db;
mod visitor;

use std::collections::HashMap;

use config::Sites;
use serde::{Deserialize, Serialize};
use worker::{
    console_error, console_log, event, Cache, Context, D1Database, Date, Env, Method, Request,
    Response, ScheduleContext, ScheduledEvent, Url,
};

/// Largest `POST /hit` body accepted, in bytes.
const MAX_BODY_BYTES: usize = 2_048;
const DEFAULT_PAGES: u32 = 10;
const MAX_PAGES: u32 = 100;
const DEFAULT_DAILY_DAYS: u32 = 30;
const DEFAULT_CACHE_SECONDS: u32 = 60;
const MAX_CACHE_SECONDS: u32 = 86_400;

const JSON: &str = "application/json";
const SVG: &str = "image/svg+xml; charset=utf-8";

type ApiResult<T> = std::result::Result<T, ApiError>;

#[event(start)]
fn start() {
    console_error_panic_hook::set_once();
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> worker::Result<Response> {
    let sites = match load_sites(&env) {
        Ok(sites) => sites,
        Err(e) => {
            console_error!("bad SITES setting: {e}");
            return ApiError::new(500, format!("bad SITES setting: {e}")).into_response();
        }
    };
    let origin = req.headers().get("Origin")?;
    let mut response = match route(req, &env, &sites, origin.as_deref()).await {
        Ok(response) => response,
        Err(e) => e.into_response()?,
    };
    // Browsers may read responses only on the configured sites' origins.
    let headers = response.headers_mut();
    if let Some(origin) = origin.filter(|o| sites.allows_any(o)) {
        headers.set("Access-Control-Allow-Origin", &origin)?;
    }
    headers.append("Vary", "Origin")?;
    Ok(response)
}

/// Runs daily from the cron in `wrangler.toml`. Deletes the salts and
/// visitor hashes of earlier days, which only today's dedupe needs.
#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    let purge = async { db::purge_before(&env.d1("DB")?, today()).await };
    match purge.await {
        Ok(deleted) => console_log!("purged {deleted} visitor hashes from earlier days"),
        Err(e) => console_error!("purge failed: {e}"),
    }
}

async fn route(
    req: Request,
    env: &Env,
    sites: &Sites,
    origin: Option<&str>,
) -> ApiResult<Response> {
    let url = req.url()?;
    let query: HashMap<String, String> = url.query_pairs().into_owned().collect();
    match (req.method(), url.path()) {
        (Method::Options, _) => preflight(sites, origin),
        (Method::Post, "/hit") => hit(req, env, sites, origin, &query).await,
        (Method::Get, "/") => Ok(Response::from_json(&ABOUT)?),
        (Method::Get, route @ ("/views" | "/pages" | "/daily" | "/badge.svg")) => {
            stats(env, sites, &url, route, &query).await
        }
        (_, "/hit") => Err(ApiError::method_not_allowed("POST, OPTIONS")),
        (_, "/" | "/views" | "/pages" | "/daily" | "/badge.svg") => {
            Err(ApiError::method_not_allowed("GET, OPTIONS"))
        }
        _ => Err(ApiError::new(404, "not found")),
    }
}

fn preflight(sites: &Sites, origin: Option<&str>) -> ApiResult<Response> {
    if !origin.is_some_and(|o| sites.allows_any(o)) {
        return Err(ApiError::new(403, "origin not allowed"));
    }
    let mut response = Response::empty()?.with_status(204);
    let headers = response.headers_mut();
    headers.set("Access-Control-Allow-Methods", "GET, POST, OPTIONS")?;
    headers.set("Access-Control-Allow-Headers", "Content-Type")?;
    headers.set("Access-Control-Max-Age", "86400")?;
    Ok(response)
}

#[derive(Default, Deserialize)]
struct HitBody {
    site: Option<String>,
    path: Option<String>,
}

#[derive(Serialize)]
struct HitResult {
    counted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'static str>,
}

/// `POST /hit`, with a JSON body `{"site": "...", "path": "/..."}` or the
/// same two fields as query parameters.
async fn hit(
    mut req: Request,
    env: &Env,
    sites: &Sites,
    origin: Option<&str>,
    query: &HashMap<String, String>,
) -> ApiResult<Response> {
    let declared = req.headers().get("Content-Length")?;
    if declared
        .and_then(|len| len.parse::<usize>().ok())
        .is_some_and(|len| len > MAX_BODY_BYTES)
    {
        return Err(ApiError::new(413, "body too large"));
    }
    let text = req.text().await?;
    if text.len() > MAX_BODY_BYTES {
        return Err(ApiError::new(413, "body too large"));
    }
    let body: HitBody = if text.trim().is_empty() {
        HitBody::default()
    } else {
        serde_json::from_str(&text).map_err(|_| {
            ApiError::bad_request(r#"body must be JSON like {"site": "blog", "path": "/about"}"#)
        })?
    };

    let site = body
        .site
        .or_else(|| query.get("site").cloned())
        .ok_or_else(|| ApiError::bad_request("site is required"))?;
    if !sites.contains(&site) {
        return Err(ApiError::unknown_site(&site));
    }
    if !origin.is_some_and(|o| sites.allows(&site, o)) {
        return Err(ApiError::new(
            403,
            format!("this origin may not count views for {site:?}"),
        ));
    }
    let path = body
        .path
        .or_else(|| query.get("path").cloned())
        .ok_or_else(|| ApiError::bad_request("path is required"))?;
    let path = visitor::normalize_path(&path).map_err(ApiError::bad_request)?;

    let headers = req.headers();
    let user_agent = headers.get("User-Agent")?.unwrap_or_default();
    if visitor::is_bot(&user_agent) {
        return counted(false, Some("bot"));
    }
    // Speculative prerenders that the visitor may never look at.
    if headers
        .get("Sec-Purpose")?
        .is_some_and(|p| p.contains("prefetch"))
    {
        return counted(false, Some("prefetch"));
    }
    let ip = headers.get("CF-Connecting-IP")?.unwrap_or_default();

    let db = env.d1("DB")?;
    let today = today();
    let salt = db::daily_salt(&db, today).await?;
    if let Ok(limiter) = env.rate_limiter("HIT_LIMITER") {
        if !limiter
            .limit(visitor::limiter_key(&salt, &ip))
            .await?
            .success
        {
            return Err(ApiError::new(
                429,
                "too many hits from this address, try again in a minute",
            ));
        }
    }
    let visitor = visitor::visitor_id(&salt, &site, &path, &ip, &user_agent);
    if !db::record_visitor(&db, today, &visitor).await? {
        return counted(false, Some("duplicate"));
    }
    db::add_view(&db, &site, &path, today).await?;
    counted(true, None)
}

fn counted(counted: bool, reason: Option<&'static str>) -> ApiResult<Response> {
    Ok(Response::from_json(&HitResult { counted, reason })?)
}

/// The read routes. Responses are cached at the edge for `CACHE_SECONDS`
/// so busy pages don't re-run the same D1 query on every load.
async fn stats(
    env: &Env,
    sites: &Sites,
    url: &Url,
    route: &str,
    query: &HashMap<String, String>,
) -> ApiResult<Response> {
    let site = query
        .get("site")
        .ok_or_else(|| ApiError::bad_request("site is required"))?;
    if !sites.contains(site) {
        return Err(ApiError::unknown_site(site));
    }

    let ttl = cache_seconds(env);
    let cache = Cache::default();
    let key = url.to_string();
    if ttl > 0 {
        if let Some(mut cached) = cache.get(&key, false).await? {
            let content_type = cached.headers().get("Content-Type")?.unwrap_or_default();
            return Ok(reply(&content_type, cached.text().await?, ttl)?);
        }
    }

    let db = env.d1("DB")?;
    let today = today();
    let (content_type, body) = match route {
        "/views" => (JSON, views(&db, site, query, today).await?),
        "/pages" => (JSON, pages(&db, site, query, today).await?),
        "/daily" => (JSON, daily(&db, site, query, today).await?),
        _ => (SVG, badge(&db, site, query, today).await?),
    };
    let mut response = reply(content_type, body, ttl)?;
    if ttl > 0 {
        if let Err(e) = cache.put(&key, response.cloned()?).await {
            console_error!("cache put failed: {e}");
        }
    }
    Ok(response)
}

fn reply(content_type: &str, body: String, ttl: u32) -> worker::Result<Response> {
    let mut response = Response::ok(body)?;
    let headers = response.headers_mut();
    headers.set("Content-Type", content_type)?;
    let cache_control = match ttl {
        0 => "no-store".to_string(),
        ttl => format!("public, max-age={ttl}"),
    };
    headers.set("Cache-Control", &cache_control)?;
    Ok(response)
}

/// The days a stats request covers: the last `days` days, or all time.
#[derive(Serialize)]
struct Span {
    days: Option<u32>,
    from: Option<String>,
    to: Option<String>,
    #[serde(skip)]
    first_day: i64,
}

impl Span {
    fn parse(
        query: &HashMap<String, String>,
        today: i64,
        default_days: Option<u32>,
    ) -> ApiResult<Self> {
        let days = match query.get("days") {
            Some(raw) => Some(day::parse_days(raw).map_err(ApiError::bad_request)?),
            None => default_days,
        };
        Ok(match days {
            Some(days) => {
                let first_day = day::window_start(today, days);
                Self {
                    days: Some(days),
                    from: Some(day::to_iso(first_day)),
                    to: Some(day::to_iso(today)),
                    first_day,
                }
            }
            // Day 0 is 1970-01-01, before any view could be stored.
            None => Self {
                days: None,
                from: None,
                to: None,
                first_day: 0,
            },
        })
    }
}

fn path_param(query: &HashMap<String, String>) -> ApiResult<Option<String>> {
    query
        .get("path")
        .map(|path| visitor::normalize_path(path))
        .transpose()
        .map_err(ApiError::bad_request)
}

async fn total(db: &D1Database, site: &str, path: Option<&str>, from: i64) -> worker::Result<u64> {
    match path {
        Some(path) => db::page_total(db, site, path, from).await,
        None => db::site_total(db, site, from).await,
    }
}

#[derive(Serialize)]
struct Views<'a> {
    site: &'a str,
    path: Option<&'a str>,
    #[serde(flatten)]
    span: Span,
    views: u64,
}

/// `GET /views?site=S[&path=P][&days=N]`
async fn views(
    db: &D1Database,
    site: &str,
    query: &HashMap<String, String>,
    today: i64,
) -> ApiResult<String> {
    let path = path_param(query)?;
    let span = Span::parse(query, today, None)?;
    let views = total(db, site, path.as_deref(), span.first_day).await?;
    Ok(serde_json::to_string(&Views {
        site,
        path: path.as_deref(),
        span,
        views,
    })
    .map_err(worker::Error::from)?)
}

#[derive(Serialize)]
struct Pages<'a> {
    site: &'a str,
    #[serde(flatten)]
    span: Span,
    pages: Vec<db::PageViews>,
}

/// `GET /pages?site=S[&days=N][&limit=L]`
async fn pages(
    db: &D1Database,
    site: &str,
    query: &HashMap<String, String>,
    today: i64,
) -> ApiResult<String> {
    let span = Span::parse(query, today, None)?;
    let limit = match query.get("limit") {
        Some(raw) => raw
            .trim()
            .parse::<u32>()
            .ok()
            .filter(|limit| (1..=MAX_PAGES).contains(limit))
            .ok_or_else(|| {
                ApiError::bad_request(format!(
                    "limit must be a whole number from 1 to {MAX_PAGES}"
                ))
            })?,
        None => DEFAULT_PAGES,
    };
    let pages = db::top_pages(db, site, span.first_day, limit).await?;
    Ok(serde_json::to_string(&Pages { site, span, pages }).map_err(worker::Error::from)?)
}

#[derive(Serialize)]
struct DayCount {
    date: String,
    views: u64,
}

#[derive(Serialize)]
struct Daily<'a> {
    site: &'a str,
    path: Option<&'a str>,
    #[serde(flatten)]
    span: Span,
    series: Vec<DayCount>,
}

/// `GET /daily?site=S[&path=P][&days=N]`, with every day in the window
/// listed, oldest first, including days without views.
async fn daily(
    db: &D1Database,
    site: &str,
    query: &HashMap<String, String>,
    today: i64,
) -> ApiResult<String> {
    let path = path_param(query)?;
    let span = Span::parse(query, today, Some(DEFAULT_DAILY_DAYS))?;
    let counts = db::daily(db, site, path.as_deref(), span.first_day, today).await?;
    let series = day::fill_series(span.first_day, today, &counts)
        .into_iter()
        .map(|(day, views)| DayCount {
            date: day::to_iso(day),
            views,
        })
        .collect();
    Ok(serde_json::to_string(&Daily {
        site,
        path: path.as_deref(),
        span,
        series,
    })
    .map_err(worker::Error::from)?)
}

/// `GET /badge.svg?site=S[&path=P][&days=N][&label=L][&color=C]`
async fn badge(
    db: &D1Database,
    site: &str,
    query: &HashMap<String, String>,
    today: i64,
) -> ApiResult<String> {
    let path = path_param(query)?;
    let span = Span::parse(query, today, None)?;
    let label = query
        .get("label")
        .map_or_else(|| "views".to_string(), |l| badge::clip_label(l));
    let color = match query.get("color") {
        Some(raw) => badge::parse_color(raw)
            .ok_or_else(|| ApiError::bad_request("color must be a hex color like 4c1 or e05d44"))?,
        None => badge::DEFAULT_COLOR.to_string(),
    };
    let views = total(db, site, path.as_deref(), span.first_day).await?;
    Ok(badge::render(&label, &badge::compact(views), &color))
}

#[derive(Serialize)]
struct About {
    name: &'static str,
    version: &'static str,
    docs: &'static str,
}

const ABOUT: About = About {
    name: env!("CARGO_PKG_NAME"),
    version: env!("CARGO_PKG_VERSION"),
    docs: env!("CARGO_PKG_REPOSITORY"),
};

fn today() -> i64 {
    day::from_millis(Date::now().as_millis())
}

/// Reads `SITES`, written in `wrangler.toml` as a table or in `.dev.vars`
/// as a JSON string.
fn load_sites(env: &Env) -> std::result::Result<Sites, String> {
    if let Ok(json) = env.var("SITES") {
        return Sites::from_json(&json.to_string());
    }
    let map = env
        .object_var::<HashMap<String, Vec<String>>>("SITES")
        .map_err(|e| format!("SITES must map site ids to lists of origins ({e})"))?;
    Sites::from_map(map)
}

fn cache_seconds(env: &Env) -> u32 {
    env.var("CACHE_SECONDS")
        .ok()
        .and_then(|v| v.to_string().trim().parse::<u32>().ok())
        .map_or(DEFAULT_CACHE_SECONDS, |ttl| ttl.min(MAX_CACHE_SECONDS))
}

#[derive(Debug)]
struct ApiError {
    status: u16,
    message: String,
    allow: Option<&'static str>,
}

#[derive(Serialize)]
struct ErrorBody<'a> {
    error: &'a str,
}

impl ApiError {
    fn new(status: u16, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            allow: None,
        }
    }

    fn bad_request(message: impl Into<String>) -> Self {
        Self::new(400, message)
    }

    fn unknown_site(site: &str) -> Self {
        Self::new(404, format!("unknown site {site:?}"))
    }

    fn method_not_allowed(allow: &'static str) -> Self {
        Self {
            allow: Some(allow),
            ..Self::new(405, "method not allowed")
        }
    }

    fn into_response(self) -> worker::Result<Response> {
        let mut response = Response::from_json(&ErrorBody {
            error: &self.message,
        })?
        .with_status(self.status);
        let headers = response.headers_mut();
        headers.set("Cache-Control", "no-store")?;
        if let Some(allow) = self.allow {
            headers.set("Allow", allow)?;
        }
        Ok(response)
    }
}

impl From<worker::Error> for ApiError {
    fn from(e: worker::Error) -> Self {
        console_error!("{e}");
        Self::new(500, "internal error")
    }
}
