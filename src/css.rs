//! Scopes a component stylesheet to one widget instance.
//!
//! GTK CSS is global to the display, so every rule is prefixed with the
//! widget's generated scope class. `:host` and `widget` both refer to the
//! widget root.

pub fn scope(css: &str, scope: &str) -> String {
    let mut out = String::with_capacity(css.len() + 64);
    let bytes = css.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Whitespace between rules.
        if bytes[i].is_ascii_whitespace() {
            out.push(bytes[i] as char);
            i += 1;
            continue;
        }

        // Comments.
        if css[i..].starts_with("/*") {
            let end = css[i..]
                .find("*/")
                .map(|e| i + e + 2)
                .unwrap_or(bytes.len());
            out.push_str(&css[i..end]);
            i = end;
            continue;
        }

        // At-rules: copy verbatim (`@define-color ...;`) or with their block.
        if bytes[i] == b'@' {
            let semi = find_unquoted(css, i, ';');
            let brace = find_unquoted(css, i, '{');

            match (semi, brace) {
                (Some(s), Some(b)) if s < b => {
                    out.push_str(&css[i..=s]);
                    i = s + 1;
                }
                (_, Some(b)) => {
                    let end = matching_brace(css, b).unwrap_or(bytes.len() - 1);
                    let name = css[i..b].split_whitespace().next().unwrap_or_default();

                    // Conditional-group rules contain ordinary selectors and
                    // must be scoped too. Keyframes and @font-face contain
                    // declarations/percentages, not widget selectors.
                    if matches!(name, "@media" | "@supports" | "@container" | "@layer") {
                        out.push_str(&css[i..=b]);
                        out.push_str(&crate::css::scope(&css[b + 1..end], scope));
                        out.push('}');
                    } else {
                        out.push_str(&css[i..=end]);
                    }
                    i = end + 1;
                }
                _ => {
                    out.push_str(&css[i..]);
                    break;
                }
            }
            continue;
        }

        // Ordinary rule: prefix its selector list.
        let Some(brace) = find_unquoted(css, i, '{') else {
            out.push_str(&css[i..]);
            break;
        };
        let prelude = &css[i..brace];
        let end = matching_brace(css, brace).unwrap_or(bytes.len() - 1);

        out.push_str(&prefix_selectors(prelude, scope));
        out.push_str(&css[brace..=end]);
        i = end + 1;
    }

    out
}

fn prefix_selectors(prelude: &str, scope: &str) -> String {
    let mut out = String::new();

    for sel in split_top_level(prelude, ',') {
        let sel = rewrite_tags(sel.trim());
        if sel.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push_str(", ");
        }
        match sel.as_str() {
            ":host" | "widget" => out.push_str(&format!(".{scope}.nitroget-root")),
            "*" => out.push_str(&format!(".{scope} *")),
            _ => out.push_str(&format!(".{scope} {sel}")),
        }
    }

    out.push(' ');
    out
}

/// Element tags are exposed as CSS classes so `<bar>` can be styled as `bar`.
fn rewrite_tags(sel: &str) -> String {
    const TAGS: &[&str] = &[
        "box", "row", "grid", "label", "bar", "slider", "button", "image", "spacer", "sep",
    ];

    sel.split_whitespace()
        .map(|part| {
            if TAGS.contains(&part) {
                format!(".{part}")
            } else {
                part.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn find_unquoted(s: &str, start: usize, needle: char) -> Option<usize> {
    let mut quote: Option<char> = None;
    let mut depth = 0i32;

    for (off, c) in s[start..].char_indices() {
        let idx = start + off;
        if let Some(q) = quote {
            if c == q && !is_escaped(s, idx) {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => depth -= 1,
            _ if c == needle && depth == 0 => return Some(idx),
            _ => {}
        }
    }
    None
}

fn matching_brace(s: &str, open: usize) -> Option<usize> {
    let mut depth = 0i32;
    let mut quote: Option<char> = None;

    for (off, c) in s[open..].char_indices() {
        let idx = open + off;
        if let Some(q) = quote {
            if c == q && !is_escaped(s, idx) {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(idx);
                }
            }
            _ => {}
        }
    }
    None
}

fn split_top_level(s: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut quote: Option<char> = None;
    let mut depth = 0i32;

    for (idx, c) in s.char_indices() {
        if let Some(q) = quote {
            if c == q && !is_escaped(s, idx) {
                quote = None;
            }
            continue;
        }
        match c {
            '"' | '\'' => quote = Some(c),
            '(' => depth += 1,
            ')' => depth -= 1,
            _ if c == sep && depth == 0 => {
                out.push(s[start..idx].to_string());
                start = idx + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(s[start..].to_string());
    out
}

fn is_escaped(s: &str, idx: usize) -> bool {
    let mut n = 0;
    let mut i = idx;
    while i > 0 && s.as_bytes()[i - 1] == b'\\' {
        n += 1;
        i -= 1;
    }
    n % 2 == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefixes_selectors() {
        let css = ".card { padding: 4px; }\nlabel, .x > span { color: red; }\nbar trough { min-height: 4px; }";
        let out = scope(css, "n-clock");
        assert!(out.contains(".n-clock .card"));
        assert!(out.contains(".n-clock .label"));
        assert!(out.contains(".n-clock .x > span"));
        assert!(out.contains(".n-clock .bar trough"));
    }

    #[test]
    fn rewrites_host() {
        assert!(scope(":host { color: red; }", "n-a").contains(".n-a.nitroget-root {"));
    }

    #[test]
    fn keeps_at_rules() {
        let out = scope("@define-color c red;\n.a { color: red; }", "s");
        assert!(out.starts_with("@define-color c red;"));
    }

    #[test]
    fn scopes_rules_inside_media_blocks() {
        let css = "@media (prefers-color-scheme: dark) { .card { color: white; } }";
        let out = scope(css, "n-clock");
        assert!(out.contains("@media (prefers-color-scheme: dark)"));
        assert!(out.contains(".n-clock .card"));
    }

    #[test]
    fn leaves_keyframes_unscoped() {
        let css = "@keyframes spin { from { opacity: 0; } to { opacity: 1; } }";
        let out = scope(css, "n-clock");
        assert!(out.contains("@keyframes spin"));
        assert!(out.contains("from { opacity: 0; }"));
        assert!(!out.contains(".n-clock from"));
    }
}
