//! Minimal Svelte-like template parser.
//!
//! Supported syntax:
//!   <element attr="value" on:click="count = count + 1">...</element>
//!   <element />
//!   { expression }
//!   {#if expression} ... {:else} ... {/if}
//!   {#each expression as item} ... {/each}
//!   {#each expression as item, index} ... {/each}
//!   \{  -> literal brace
//!
//! Control-flow blocks and interpolations are evaluated later, against the
//! widget's Lua globals.

use anyhow::{Result, anyhow};

#[derive(Debug, Clone)]
pub struct Element {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Element {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

#[derive(Debug, Clone)]
pub enum Node {
    Element(Element),
    /// Raw text, may contain `{ expression }` interpolations.
    Text(String),
    If {
        cond: String,
        then_body: Vec<Node>,
        else_body: Vec<Node>,
    },
    Each {
        list: String,
        item: String,
        index: Option<String>,
        body: Vec<Node>,
    },
}

pub fn parse(src: &str) -> Result<Vec<Node>> {
    Parser::new(src).parse_nodes(&[])
}

struct Parser<'a> {
    src: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a str) -> Self {
        Self { src, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn line(&self) -> usize {
        self.src[..self.pos.min(self.src.len())]
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
            + 1
    }

    fn err<T>(&self, msg: impl std::fmt::Display) -> Result<T> {
        Err(anyhow!("line {}: {}", self.line(), msg))
    }

    fn skip_ws(&mut self) {
        while let Some(c) = self.rest().chars().next() {
            if c.is_whitespace() {
                self.pos += c.len_utf8();
            } else {
                break;
            }
        }
    }

    fn parse_nodes(&mut self, stops: &[&str]) -> Result<Vec<Node>> {
        let mut out: Vec<Node> = Vec::new();

        loop {
            if self.pos >= self.src.len() {
                break;
            }
            let rest = self.rest();

            if stops.iter().any(|s| rest.starts_with(s)) {
                break;
            }
            if rest.starts_with("{/") || rest.starts_with("{:") {
                break;
            }

            // Comments.
            if rest.starts_with("<!--") {
                match rest.find("-->") {
                    Some(i) => {
                        self.pos += i + 3;
                        continue;
                    }
                    None => return self.err("unterminated comment"),
                }
            }

            // {#if ...}
            if let Some(after) = rest.strip_prefix("{#if ") {
                let end = match after.find('}') {
                    Some(e) => e,
                    None => return self.err("unterminated {#if}"),
                };
                let cond = after[..end].trim().to_string();
                self.pos += "{#if ".len() + end + 1;

                let then_body = self.parse_nodes(&["{:else}", "{/if}"])?;
                let mut else_body = Vec::new();

                if self.rest().starts_with("{:else}") {
                    self.pos += "{:else}".len();
                    else_body = self.parse_nodes(&["{/if}"])?;
                }

                if !self.rest().starts_with("{/if}") {
                    return self.err("expected {/if}");
                }
                self.pos += "{/if}".len();

                out.push(Node::If {
                    cond,
                    then_body,
                    else_body,
                });
                continue;
            }

            // {#each ...}
            if let Some(after) = rest.strip_prefix("{#each ") {
                let end = match after.find('}') {
                    Some(e) => e,
                    None => return self.err("unterminated {#each}"),
                };
                let header = after[..end].trim().to_string();
                self.pos += "{#each ".len() + end + 1;

                let (list, binding) = match header.split_once(" as ") {
                    Some((l, b)) => (l.trim().to_string(), b.trim().to_string()),
                    None => return self.err("{#each ... as item} expected"),
                };
                let (item, index) = match binding.split_once(',') {
                    Some((i, n)) => (i.trim().to_string(), Some(n.trim().to_string())),
                    None => (binding, None),
                };

                let body = self.parse_nodes(&["{/each}"])?;
                if !self.rest().starts_with("{/each}") {
                    return self.err("expected {/each}");
                }
                self.pos += "{/each}".len();

                out.push(Node::Each {
                    list,
                    item,
                    index,
                    body,
                });
                continue;
            }

            // Element.
            if rest.starts_with('<') {
                out.push(self.parse_element()?);
                continue;
            }

            // Interpolation, merged into the preceding text node.
            if rest.starts_with('{') {
                match rest.find('}') {
                    Some(end) => {
                        let seg = &rest[..=end];
                        self.pos += end + 1;
                        if let Some(Node::Text(t)) = out.last_mut() {
                            t.push_str(seg);
                        } else {
                            out.push(Node::Text(seg.to_string()));
                        }
                        continue;
                    }
                    None => return self.err("unterminated { expression }"),
                }
            }

            // Plain text.
            let start = self.pos;
            while let Some(c) = self.rest().chars().next() {
                if c == '\\' && self.rest().starts_with("\\{") {
                    self.pos += 2;
                    continue;
                }
                if c == '<' || c == '{' {
                    break;
                }
                self.pos += c.len_utf8();
            }
            let text = &self.src[start..self.pos];
            if !text.is_empty() {
                if text.trim().is_empty() {
                    // Keep inline spacing between expressions (`{a} {b}`), but
                    // drop pretty-printing indentation that includes a newline.
                    if !text.contains('\n')
                        && let Some(Node::Text(previous)) = out.last_mut()
                    {
                        previous.push_str(text);
                    }
                } else {
                    out.push(Node::Text(text.to_string()));
                }
            }
        }

        Ok(out)
    }

    fn parse_element(&mut self) -> Result<Node> {
        self.pos += 1; // '<'

        let start = self.pos;
        while let Some(c) = self.rest().chars().next() {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == ':' {
                self.pos += 1;
            } else {
                break;
            }
        }
        let tag = self.src[start..self.pos].to_string();
        if tag.is_empty() {
            return self.err("expected an element name");
        }

        let mut attrs = Vec::new();
        let self_closing;

        loop {
            self.skip_ws();

            if self.rest().starts_with("/>") {
                self.pos += 2;
                self_closing = true;
                break;
            }
            if self.rest().starts_with('>') {
                self.pos += 1;
                self_closing = false;
                break;
            }
            if self.pos >= self.src.len() {
                return self.err(format!("unterminated <{tag}>"));
            }

            let name_start = self.pos;
            while let Some(c) = self.rest().chars().next() {
                if c.is_whitespace() || c == '=' || c == '>' || c == '/' {
                    break;
                }
                self.pos += c.len_utf8();
            }
            if self.pos == name_start {
                return self.err(format!("unexpected character in <{tag}>"));
            }
            let name = self.src[name_start..self.pos].to_string();

            self.skip_ws();
            let mut value = String::new();

            if self.rest().starts_with('=') {
                self.pos += 1;
                self.skip_ws();

                match self.rest().chars().next() {
                    Some(q @ ('"' | '\'')) => {
                        self.pos += 1;
                        let vstart = self.pos;
                        match self.src[vstart..].find(q) {
                            Some(i) => {
                                value = self.src[vstart..vstart + i].to_string();
                                self.pos = vstart + i + 1;
                            }
                            None => return self.err(format!("unterminated value of `{name}`")),
                        }
                    }
                    _ => {
                        let vstart = self.pos;
                        while let Some(c) = self.rest().chars().next() {
                            if c.is_whitespace() || c == '>' {
                                break;
                            }
                            self.pos += c.len_utf8();
                        }
                        value = self.src[vstart..self.pos].to_string();
                    }
                }
            }

            attrs.push((name, value));
        }

        if self_closing {
            return Ok(Node::Element(Element {
                tag,
                attrs,
                children: Vec::new(),
            }));
        }

        let close = format!("</{tag}>");
        let children = self.parse_nodes(&[close.as_str()])?;

        if !self.rest().starts_with(&close) {
            return self.err(format!("expected {close}"));
        }
        self.pos += close.len();

        Ok(Node::Element(Element {
            tag,
            attrs,
            children,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_elements_and_attributes() {
        let nodes = parse(r#"<box class="card" gap="8"><label>x</label></box>"#).unwrap();
        match &nodes[0] {
            Node::Element(el) => {
                assert_eq!(el.tag, "box");
                assert_eq!(el.attr("class"), Some("card"));
                assert_eq!(el.attr("gap"), Some("8"));
                assert_eq!(el.children.len(), 1);
            }
            _ => panic!("expected element"),
        }
    }

    #[test]
    fn parses_if_else() {
        let nodes = parse("{#if a}yes{:else}no{/if}").unwrap();
        match &nodes[0] {
            Node::If {
                cond,
                then_body,
                else_body,
            } => {
                assert_eq!(cond, "a");
                assert_eq!(then_body.len(), 1);
                assert_eq!(else_body.len(), 1);
            }
            _ => panic!("expected if"),
        }
    }

    #[test]
    fn parses_each_with_index() {
        let nodes = parse("{#each xs as x, i}<label>{x}</label>{/each}").unwrap();
        match &nodes[0] {
            Node::Each { item, index, .. } => {
                assert_eq!(item, "x");
                assert_eq!(index.as_deref(), Some("i"));
            }
            _ => panic!("expected each"),
        }
    }

    #[test]
    fn keeps_inline_spaces_between_interpolations() {
        let nodes = parse("<label>{a} {b}</label>").unwrap();
        match &nodes[0] {
            Node::Element(el) => {
                assert_eq!(el.children.len(), 1);
                assert!(matches!(&el.children[0], Node::Text(text) if text == "{a} {b}"));
            }
            _ => panic!("expected element"),
        }
    }

    #[test]
    fn keeps_interpolation_in_text() {
        let nodes = parse("<label>hi {name}</label>").unwrap();
        match &nodes[0] {
            Node::Element(el) => {
                assert!(matches!(&el.children[0], Node::Text(text) if text == "hi {name}"));
            }
            _ => panic!("expected element"),
        }
    }

    #[test]
    fn supports_self_closing_and_escaped_braces() {
        let nodes = parse(r#"<box><spacer /><label>\{literal}</label></box>"#).unwrap();
        match &nodes[0] {
            Node::Element(el) => {
                assert_eq!(el.children.len(), 2);
                match &el.children[1] {
                    Node::Element(label) => {
                        assert!(matches!(&label.children[0], Node::Text(t) if t == r"\{literal}"));
                    }
                    _ => panic!("expected label"),
                }
            }
            _ => panic!("expected element"),
        }
    }
}
