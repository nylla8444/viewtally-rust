-- Days are UTC day numbers: whole days since 1970-01-01.
-- Every table is WITHOUT ROWID with no extra indexes, so each upsert
-- writes exactly one row on D1's meter.

-- Views of each page per day.
CREATE TABLE page_views (
    site  TEXT    NOT NULL,
    path  TEXT    NOT NULL,
    day   INTEGER NOT NULL,
    views INTEGER NOT NULL,
    PRIMARY KEY (site, path, day)
) WITHOUT ROWID;

-- Views of each site per day. Kept next to page_views so a site's
-- 365-day total reads at most 365 rows, however many pages it has.
CREATE TABLE site_views (
    site  TEXT    NOT NULL,
    day   INTEGER NOT NULL,
    views INTEGER NOT NULL,
    PRIMARY KEY (site, day)
) WITHOUT ROWID;

-- Hashes of today's visitors, used only to skip repeat views.
-- The daily cron deletes earlier days.
CREATE TABLE visitors (
    day  INTEGER NOT NULL,
    hash TEXT    NOT NULL,
    PRIMARY KEY (day, hash)
) WITHOUT ROWID;

-- The random salt of each day's visitor hashes. The daily cron deletes
-- earlier days, after which those hashes can't be linked to anyone.
CREATE TABLE salts (
    day  INTEGER NOT NULL PRIMARY KEY,
    salt TEXT    NOT NULL
) WITHOUT ROWID;
