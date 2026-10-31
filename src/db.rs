//! Every D1 query. The schema is in `migrations/`.

use std::cell::RefCell;

use serde::{Deserialize, Serialize};
use worker::wasm_bindgen::JsValue;
use worker::{D1Database, D1Result, Result};

/// Visitor rows deleted per statement when purging, to stay well under
/// D1's per-query limits.
const PURGE_CHUNK: usize = 5_000;
/// At most this many purge statements per run. Workers Free allows 50 D1
/// queries per invocation.
const PURGE_ROUNDS: usize = 40;

thread_local! {
    /// Today's salt, kept for the life of the isolate to save a query per hit.
    static SALT: RefCell<Option<(i64, String)>> = const { RefCell::new(None) };
}

#[derive(Debug, Deserialize, Serialize)]
pub struct PageViews {
    pub path: String,
    pub views: u64,
}

#[derive(Deserialize)]
struct DayViews {
    day: i64,
    views: u64,
}

#[derive(Deserialize)]
struct SaltRow {
    salt: String,
}

/// D1 binds JavaScript numbers. An `i64` would become a BigInt, which D1
/// rejects, and day numbers are far inside f64's exact range.
fn num(n: i64) -> JsValue {
    JsValue::from_f64(n as f64)
}

fn changes(result: &D1Result) -> Result<usize> {
    Ok(result.meta()?.and_then(|meta| meta.changes).unwrap_or(0))
}

/// The salt for `day`'s visitor hashes. The first isolate to need it
/// generates it; later ones read the stored value.
pub async fn daily_salt(db: &D1Database, day: i64) -> Result<String> {
    let cached = SALT.with_borrow(|salt| {
        salt.as_ref()
            .filter(|(salt_day, _)| *salt_day == day)
            .map(|(_, salt)| salt.clone())
    });
    if let Some(salt) = cached {
        return Ok(salt);
    }

    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| format!("no random source: {e}"))?;
    let fresh = crate::visitor::hex(&bytes);
    let results = db
        .batch(vec![
            db.prepare("INSERT OR IGNORE INTO salts (day, salt) VALUES (?1, ?2)")
                .bind(&[num(day), fresh.into()])?,
            db.prepare("SELECT salt FROM salts WHERE day = ?1")
                .bind(&[num(day)])?,
        ])
        .await?;
    let salt = results
        .get(1)
        .map(|stored| stored.results::<SaltRow>())
        .transpose()?
        .and_then(|rows| rows.into_iter().next())
        .ok_or("the salt for today was not stored")?
        .salt;
    SALT.set(Some((day, salt.clone())));
    Ok(salt)
}

/// Records that a visitor was seen today. Returns `false` if they already
/// were, meaning the view is a repeat.
pub async fn record_visitor(db: &D1Database, day: i64, visitor: &str) -> Result<bool> {
    let result = db
        .prepare("INSERT OR IGNORE INTO visitors (day, hash) VALUES (?1, ?2)")
        .bind(&[num(day), visitor.into()])?
        .run()
        .await?;
    Ok(changes(&result)? > 0)
}

/// Adds one view to the page's and the site's count for `day`.
pub async fn add_view(db: &D1Database, site: &str, path: &str, day: i64) -> Result<()> {
    db.batch(vec![
        db.prepare(
            "INSERT INTO page_views (site, path, day, views) VALUES (?1, ?2, ?3, 1) \
             ON CONFLICT (site, path, day) DO UPDATE SET views = views + 1",
        )
        .bind(&[site.into(), path.into(), num(day)])?,
        db.prepare(
            "INSERT INTO site_views (site, day, views) VALUES (?1, ?2, 1) \
             ON CONFLICT (site, day) DO UPDATE SET views = views + 1",
        )
        .bind(&[site.into(), num(day)])?,
    ])
    .await?;
    Ok(())
}

/// Views of a whole site from day `from` on.
pub async fn site_total(db: &D1Database, site: &str, from: i64) -> Result<u64> {
    let views = db
        .prepare(
            "SELECT COALESCE(SUM(views), 0) AS views FROM site_views WHERE site = ?1 AND day >= ?2",
        )
        .bind(&[site.into(), num(from)])?
        .first::<u64>(Some("views"))
        .await?;
    Ok(views.unwrap_or(0))
}

/// Views of one page from day `from` on.
pub async fn page_total(db: &D1Database, site: &str, path: &str, from: i64) -> Result<u64> {
    let views = db
        .prepare(
            "SELECT COALESCE(SUM(views), 0) AS views FROM page_views \
             WHERE site = ?1 AND path = ?2 AND day >= ?3",
        )
        .bind(&[site.into(), path.into(), num(from)])?
        .first::<u64>(Some("views"))
        .await?;
    Ok(views.unwrap_or(0))
}

/// The most viewed pages from day `from` on, most views first.
pub async fn top_pages(
    db: &D1Database,
    site: &str,
    from: i64,
    limit: u32,
) -> Result<Vec<PageViews>> {
    db.prepare(
        "SELECT path, SUM(views) AS views FROM page_views WHERE site = ?1 AND day >= ?2 \
         GROUP BY path ORDER BY views DESC, path LIMIT ?3",
    )
    .bind(&[site.into(), num(from), num(i64::from(limit))])?
    .all()
    .await?
    .results()
}

/// Views per day from `from` to `to`, for the site or one of its pages.
/// Days without views are left out.
pub async fn daily(
    db: &D1Database,
    site: &str,
    path: Option<&str>,
    from: i64,
    to: i64,
) -> Result<Vec<(i64, u64)>> {
    let statement = match path {
        Some(path) => db
            .prepare(
                "SELECT day, views FROM page_views \
                 WHERE site = ?1 AND path = ?2 AND day BETWEEN ?3 AND ?4 ORDER BY day",
            )
            .bind(&[site.into(), path.into(), num(from), num(to)])?,
        None => db
            .prepare(
                "SELECT day, views FROM site_views \
                 WHERE site = ?1 AND day BETWEEN ?2 AND ?3 ORDER BY day",
            )
            .bind(&[site.into(), num(from), num(to)])?,
    };
    let rows = statement.all().await?.results::<DayViews>()?;
    Ok(rows.into_iter().map(|row| (row.day, row.views)).collect())
}

/// Deletes the salts and visitor hashes of every day before `today`.
/// Returns how many visitor hashes were deleted.
pub async fn purge_before(db: &D1Database, today: i64) -> Result<usize> {
    db.prepare("DELETE FROM salts WHERE day < ?1")
        .bind(&[num(today)])?
        .run()
        .await?;
    let mut deleted = 0;
    for _ in 0..PURGE_ROUNDS {
        let result = db
            .prepare(
                "DELETE FROM visitors WHERE (day, hash) IN \
                 (SELECT day, hash FROM visitors WHERE day < ?1 LIMIT ?2)",
            )
            .bind(&[num(today), num(PURGE_CHUNK as i64)])?
            .run()
            .await?;
        let removed = changes(&result)?;
        deleted += removed;
        if removed < PURGE_CHUNK {
            break;
        }
    }
    Ok(deleted)
}
