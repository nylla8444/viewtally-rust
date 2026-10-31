//! Turning one request into a countable view: a clean path, a bot check,
//! and the hashes that stand in for the visitor.

use sha2::{Digest, Sha256};

/// Longest path, in bytes, that is counted.
pub const MAX_PATH_LEN: usize = 256;

/// Cleans up a page path so `/about`, `/about/` and `/about?ref=x` count as
/// one page. The query string and fragment are dropped, repeated slashes
/// are merged, and a trailing slash is removed (except from `/` itself).
pub fn normalize_path(raw: &str) -> Result<String, String> {
    let raw = raw.trim();
    let end = raw.find(['?', '#']).unwrap_or(raw.len());
    let raw = &raw[..end];
    if !raw.starts_with('/') {
        return Err("path must start with '/'".into());
    }
    if raw.chars().any(char::is_control) {
        return Err("path must not contain control characters".into());
    }
    let mut path = String::with_capacity(raw.len());
    for segment in raw.split('/').filter(|s| !s.is_empty()) {
        path.push('/');
        path.push_str(segment);
    }
    if path.is_empty() {
        path.push('/');
    }
    if path.len() > MAX_PATH_LEN {
        return Err(format!("path must be at most {MAX_PATH_LEN} bytes"));
    }
    Ok(path)
}

/// Words in the user agents of crawlers, link previewers and headless
/// browsers that run JavaScript and would otherwise send hits.
const BOT_MARKERS: &[&str] = &[
    "bot",
    "crawl",
    "spider",
    "slurp",
    "headless",
    "lighthouse",
    "pagespeed",
    "prerender",
    "phantomjs",
    "puppeteer",
    "playwright",
    "facebookexternalhit",
    "embedly",
    "preview",
    "screenshot",
    "monitor",
];

/// Whether a user agent looks automated. An empty one counts as automated.
pub fn is_bot(user_agent: &str) -> bool {
    let ua = user_agent.trim().to_ascii_lowercase();
    ua.is_empty() || BOT_MARKERS.iter().any(|marker| ua.contains(marker))
}

/// A SHA-256 hash of `parts`, shortened to 128 bits and hex-encoded.
///
/// The parts are joined with NUL bytes, which none of them can contain, so
/// different splits of the same text can't produce the same hash.
pub fn digest(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            hasher.update([0]);
        }
        hasher.update(part.as_bytes());
    }
    hex(&hasher.finalize()[..16])
}

/// The visitor hash used to skip repeat views of one page on one day. It
/// changes with the daily salt, so the same person can't be followed from
/// one day to the next.
pub fn visitor_id(salt: &str, site: &str, path: &str, ip: &str, user_agent: &str) -> String {
    digest(&[salt, site, path, ip, user_agent])
}

/// The rate-limiter key for an IP address, salted like the visitor hash so
/// the raw address never leaves the Worker.
pub fn limiter_key(salt: &str, ip: &str) -> String {
    digest(&[salt, "limit", ip])
}

pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(DIGITS[usize::from(b >> 4)] as char);
        out.push(DIGITS[usize::from(b & 0xf)] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIREFOX: &str =
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:140.0) Gecko/20100101 Firefox/140.0";

    #[test]
    fn paths_are_normalized() {
        for (raw, want) in [
            ("/", "/"),
            ("/about", "/about"),
            ("/about/", "/about"),
            ("//about//team/", "/about/team"),
            ("/blog?page=2", "/blog"),
            ("/blog/#comments", "/blog"),
            ("/?utm_source=x", "/"),
            ("  /projects  ", "/projects"),
            ("/caf%C3%A9", "/caf%C3%A9"),
            ("/About", "/About"),
        ] {
            assert_eq!(normalize_path(raw).unwrap(), want, "normalizing {raw:?}");
        }
    }

    #[test]
    fn bad_paths_are_rejected() {
        assert!(normalize_path("").is_err());
        assert!(normalize_path("about").is_err());
        assert!(normalize_path("https://example.com/about").is_err());
        assert!(normalize_path("/a\nb").is_err());
        assert!(normalize_path(&format!("/{}", "a".repeat(MAX_PATH_LEN))).is_err());
        assert!(normalize_path(&format!("/{}", "a".repeat(MAX_PATH_LEN - 1))).is_ok());
    }

    #[test]
    fn browsers_are_not_bots() {
        assert!(!is_bot(FIREFOX));
        assert!(!is_bot(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1"
        ));
    }

    #[test]
    fn bots_are_bots() {
        for ua in [
            "",
            "   ",
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
            "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/140.0.0.0 Safari/537.36",
            "facebookexternalhit/1.1 (+http://www.facebook.com/externalhit_uatext.php)",
            "Mozilla/5.0 (Linux; Android 11; moto g power (2022)) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Mobile Safari/537.36 Chrome-Lighthouse",
            "Mozilla/5.0 (compatible; YandexSpider/3.0)",
        ] {
            assert!(is_bot(ua), "{ua:?} should be a bot");
        }
    }

    #[test]
    fn digest_is_128_bit_hex() {
        let id = digest(&["a", "b"]);
        assert_eq!(id.len(), 32);
        assert!(id
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(id, digest(&["a", "b"]));
    }

    #[test]
    fn digest_separates_parts() {
        assert_ne!(digest(&["ab", "c"]), digest(&["a", "bc"]));
        assert_ne!(digest(&["a", ""]), digest(&["a"]));
    }

    #[test]
    fn digest_matches_sha256() {
        // SHA-256("abc") starts with ba7816bf8f01cfea414140de5dae2223.
        assert_eq!(digest(&["abc"]), "ba7816bf8f01cfea414140de5dae2223");
    }

    #[test]
    fn visitor_ids_depend_on_every_input() {
        let base = visitor_id("salt", "blog", "/", "203.0.113.7", FIREFOX);
        assert_eq!(
            base,
            visitor_id("salt", "blog", "/", "203.0.113.7", FIREFOX)
        );
        assert_ne!(
            base,
            visitor_id("other salt", "blog", "/", "203.0.113.7", FIREFOX)
        );
        assert_ne!(
            base,
            visitor_id("salt", "docs", "/", "203.0.113.7", FIREFOX)
        );
        assert_ne!(
            base,
            visitor_id("salt", "blog", "/about", "203.0.113.7", FIREFOX)
        );
        assert_ne!(
            base,
            visitor_id("salt", "blog", "/", "203.0.113.8", FIREFOX)
        );
        assert_ne!(
            base,
            visitor_id("salt", "blog", "/", "203.0.113.7", "Other/1.0")
        );
    }

    #[test]
    fn limiter_key_depends_on_salt_and_ip() {
        let key = limiter_key("salt", "203.0.113.7");
        assert_eq!(key, limiter_key("salt", "203.0.113.7"));
        assert_ne!(key, limiter_key("salt", "203.0.113.8"));
        assert_ne!(key, limiter_key("other salt", "203.0.113.7"));
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(hex(&[0x00, 0x0f, 0xa5, 0xff]), "000fa5ff");
        assert_eq!(hex(&[]), "");
    }
}
