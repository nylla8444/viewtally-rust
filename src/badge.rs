//! A flat SVG badge, like the ones in project READMEs.

pub const DEFAULT_COLOR: &str = "007ec6";
pub const MAX_LABEL_CHARS: usize = 40;

/// Renders a two-part badge: a grey label and a colored value.
/// `color` must already be a validated hex color, without the `#`.
pub fn render(label: &str, value: &str, color: &str) -> String {
    let label = escape(label);
    let value = escape(value);
    let label_width = text_width(&label) + 10;
    let value_width = text_width(&value) + 10;
    let width = label_width + value_width;
    let label_x = label_width as f32 / 2.0;
    let value_x = label_width as f32 + value_width as f32 / 2.0;
    format!(
        concat!(
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="20" role="img" aria-label="{l}: {v}">"##,
            r##"<title>{l}: {v}</title>"##,
            r##"<linearGradient id="s" x2="0" y2="100%"><stop offset="0" stop-color="#bbb" stop-opacity=".1"/><stop offset="1" stop-opacity=".1"/></linearGradient>"##,
            r##"<clipPath id="r"><rect width="{w}" height="20" rx="3" fill="#fff"/></clipPath>"##,
            r##"<g clip-path="url(#r)"><rect width="{lw}" height="20" fill="#555"/><rect x="{lw}" width="{vw}" height="20" fill="#{c}"/><rect width="{w}" height="20" fill="url(#s)"/></g>"##,
            r##"<g fill="#fff" text-anchor="middle" font-family="Verdana,Geneva,DejaVu Sans,sans-serif" font-size="11">"##,
            r##"<text x="{lx}" y="15" fill="#010101" fill-opacity=".3">{l}</text><text x="{lx}" y="14">{l}</text>"##,
            r##"<text x="{vx}" y="15" fill="#010101" fill-opacity=".3">{v}</text><text x="{vx}" y="14">{v}</text>"##,
            r##"</g></svg>"##,
        ),
        w = width,
        lw = label_width,
        vw = value_width,
        lx = label_x,
        vx = value_x,
        l = label,
        v = value,
        c = color,
    )
}

/// Shortens a count the way badges usually show it: `999`, `1.2k`, `12k`,
/// `3.4M`. It rounds down, so a badge never shows more views than there are.
pub fn compact(n: u64) -> String {
    for (size, unit) in [(1_000_000_000, "B"), (1_000_000, "M"), (1_000, "k")] {
        if n >= size {
            let tenths = n / (size / 10);
            return if tenths < 100 && tenths % 10 != 0 {
                format!("{}.{}{unit}", tenths / 10, tenths % 10)
            } else {
                format!("{}{unit}", n / size)
            };
        }
    }
    n.to_string()
}

/// Accepts a 3- or 6-digit hex color, with or without a leading `#`.
pub fn parse_color(raw: &str) -> Option<String> {
    let hex = raw.trim().trim_start_matches('#');
    ((hex.len() == 3 || hex.len() == 6) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| hex.to_ascii_lowercase())
}

/// Cuts a label down to [`MAX_LABEL_CHARS`] characters.
pub fn clip_label(raw: &str) -> String {
    raw.trim().chars().take(MAX_LABEL_CHARS).collect()
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Approximate width in pixels of 11px Verdana. Entities such as `&amp;`
/// are measured as the one character they stand for.
fn text_width(escaped: &str) -> u32 {
    let mut width = 0;
    let mut chars = escaped.chars();
    while let Some(c) = chars.next() {
        let c = if c == '&' {
            chars.by_ref().take_while(|&c| c != ';').for_each(drop);
            'W'
        } else {
            c
        };
        width += match c {
            'i' | 'j' | 'l' | '.' | ',' | ':' | ';' | '!' | '|' | '\'' => 4,
            'f' | 'r' | 't' | 'I' | ' ' | '(' | ')' | '[' | ']' | '-' => 5,
            'm' | 'w' | 'M' | 'W' => 11,
            'A'..='Z' => 8,
            _ => 7,
        };
    }
    width
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compact_counts() {
        for (n, want) in [
            (0, "0"),
            (999, "999"),
            (1_000, "1k"),
            (1_049, "1k"),
            (1_250, "1.2k"),
            (9_999, "9.9k"),
            (10_000, "10k"),
            (12_345, "12k"),
            (999_999, "999k"),
            (1_000_000, "1M"),
            (3_456_789, "3.4M"),
            (1_500_000_000, "1.5B"),
            (u64::MAX, "18446744073B"),
        ] {
            assert_eq!(compact(n), want, "compact({n})");
        }
    }

    #[test]
    fn colors() {
        assert_eq!(parse_color("4c1").as_deref(), Some("4c1"));
        assert_eq!(parse_color("#E05D44").as_deref(), Some("e05d44"));
        for bad in ["", "12", "1234", "ggg", "red", "12345g", "url(#x)"] {
            assert_eq!(parse_color(bad), None, "{bad:?} should be rejected");
        }
    }

    #[test]
    fn labels_are_clipped() {
        assert_eq!(clip_label("  views  "), "views");
        assert_eq!(
            clip_label(&"x".repeat(100)).chars().count(),
            MAX_LABEL_CHARS
        );
    }

    #[test]
    fn badge_escapes_text() {
        let svg = render("<script>&\"", "1k", DEFAULT_COLOR);
        assert!(!svg.contains("<script>"));
        assert!(svg.contains("&lt;script&gt;&amp;&quot;"));
    }

    #[test]
    fn badge_shape() {
        let svg = render("views", "1.2k", "4c1");
        assert!(svg.starts_with("<svg xmlns=\"http://www.w3.org/2000/svg\""));
        assert!(svg.ends_with("</svg>"));
        assert!(svg.contains(r#"aria-label="views: 1.2k""#));
        assert!(svg.contains(r##"fill="#4c1""##));
        // label 5 chars wide + value 4 chars, plus 10px padding each.
        let label = text_width("views") + 10;
        let value = text_width("1.2k") + 10;
        assert!(svg.contains(&format!(
            r#"width="{}" height="20" role="img""#,
            label + value
        )));
    }

    #[test]
    fn entities_measure_as_one_character() {
        assert_eq!(text_width("&amp;"), text_width("W"));
        assert_eq!(text_width("a&lt;b"), text_width("aWb"));
    }
}
