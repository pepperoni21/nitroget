use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};

pub fn to_pango(source: &str) -> String {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);

    let parser = Parser::new_ext(source, options);
    let mut out = String::new();
    let mut lists: Vec<Option<u64>> = Vec::new();

    for event in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    out.push_str("<span weight=\"bold\" size=\"");
                    out.push_str(heading_size(level));
                    out.push_str("\">");
                }
                Tag::BlockQuote(_) => out.push_str("<i>"),
                Tag::CodeBlock(kind) => {
                    if matches!(kind, CodeBlockKind::Fenced(_)) {
                        out.push('\n');
                    }
                    out.push_str("<tt>");
                }
                Tag::List(start) => lists.push(start),
                Tag::Item => {
                    let depth = lists.len().saturating_sub(1);
                    out.push_str(&"  ".repeat(depth));
                    match lists.last_mut() {
                        Some(Some(number)) => {
                            out.push_str(&format!("{number}. "));
                            *number += 1;
                        }
                        _ => out.push_str("• "),
                    }
                }
                Tag::Emphasis => out.push_str("<i>"),
                Tag::Strong => out.push_str("<b>"),
                Tag::Strikethrough => out.push_str("<s>"),
                Tag::Link { dest_url, .. } => {
                    out.push_str("<a href=\"");
                    out.push_str(&escape_attribute(&dest_url));
                    out.push_str("\">");
                }
                Tag::Image { dest_url, .. } => {
                    out.push('[');
                    out.push_str(&escape_text(&dest_url));
                    out.push_str("] ");
                }
                Tag::Superscript => out.push_str("<sup>"),
                Tag::Subscript => out.push_str("<sub>"),
                Tag::Table(_) => out.push('\n'),
                Tag::TableRow => out.push_str("| "),
                Tag::TableCell => {}
                Tag::FootnoteDefinition(_) => {
                    out.push_str("\n[");
                }
                Tag::DefinitionList | Tag::DefinitionListTitle | Tag::DefinitionListDefinition => {
                    out.push('\n');
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => out.push_str("\n\n"),
                TagEnd::Heading(_) => out.push_str("</span>\n\n"),
                TagEnd::BlockQuote(_) => out.push_str("</i>\n"),
                TagEnd::CodeBlock => out.push_str("</tt>\n"),
                TagEnd::List(_) => {
                    lists.pop();
                    out.push('\n');
                }
                TagEnd::Item => out.push('\n'),
                TagEnd::Emphasis => out.push_str("</i>"),
                TagEnd::Strong => out.push_str("</b>"),
                TagEnd::Strikethrough => out.push_str("</s>"),
                TagEnd::Link => out.push_str("</a>"),
                TagEnd::Image => out.push(']'),
                TagEnd::Superscript => out.push_str("</sup>"),
                TagEnd::Subscript => out.push_str("</sub>"),
                TagEnd::TableRow => out.push_str("|\n"),
                TagEnd::Table => out.push('\n'),
                TagEnd::FootnoteDefinition => out.push_str("]\n"),
                TagEnd::DefinitionList
                | TagEnd::DefinitionListTitle
                | TagEnd::DefinitionListDefinition => out.push('\n'),
                _ => {}
            },
            Event::Text(text) => out.push_str(&escape_text(&text)),
            Event::Code(code) => {
                out.push_str("<tt>");
                out.push_str(&escape_text(&code));
                out.push_str("</tt>");
            }
            Event::Html(html) | Event::InlineHtml(html) => out.push_str(&escape_text(&html)),
            Event::SoftBreak | Event::HardBreak => out.push('\n'),
            Event::Rule => out.push_str("\n────────\n"),
            Event::TaskListMarker(checked) => {
                out.push_str(if checked { "[x] " } else { "[ ] " });
            }
            Event::FootnoteReference(name) => {
                out.push('[');
                out.push_str(&escape_text(&name));
                out.push(']');
            }
            Event::InlineMath(math) => {
                out.push_str("<tt>");
                out.push_str(&escape_text(&math));
                out.push_str("</tt>");
            }
            Event::DisplayMath(math) => {
                out.push_str("\n<tt>");
                out.push_str(&escape_text(&math));
                out.push_str("</tt>\n");
            }
        }
    }

    out
}

fn heading_size(level: HeadingLevel) -> &'static str {
    match level {
        HeadingLevel::H1 => "x-large",
        HeadingLevel::H2 => "large",
        HeadingLevel::H3 => "medium",
        _ => "small",
    }
}

fn escape_text(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(character),
        }
    }
    out
}

fn escape_attribute(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(character),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_inline_markdown() {
        let output = to_pango("Hello **world** and `code`.");
        assert!(output.contains("<b>world</b>"));
        assert!(output.contains("<tt>code</tt>"));
    }

    #[test]
    fn escapes_html_and_model_text() {
        let output = to_pango("<script>alert('&')</script>");
        assert!(output.contains("&lt;script&gt;"));
        assert!(output.contains("&amp;"));
        assert!(!output.contains("<script>"));
    }

    #[test]
    fn renders_links_lists_and_code_blocks() {
        let output = to_pango("- one\n- two\n\n[site](https://example.com)\n\n```\nx < y\n```");
        assert!(output.contains("• one"));
        assert!(output.contains("<a href=\"https://example.com\">site</a>"));
        assert!(output.contains("<tt>x &lt; y"));
    }
}
