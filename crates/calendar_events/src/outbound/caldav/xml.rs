//! The WebDAV multistatus subset CalDAV answers with, read by local name.
//!
//! Namespaces are dropped: every DAV and CalDAV element this adapter reads has
//! a local name no other namespace in a response reuses for something else.

/// One element, its children, and its text with entities decoded.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Node {
    pub(crate) name: String,
    pub(crate) attrs: Vec<(String, String)>,
    pub(crate) children: Vec<Node>,
    pub(crate) text: String,
}

impl Node {
    /// The value of the attribute with local name `name`.
    pub(crate) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The first child named `name`.
    pub(crate) fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|node| node.name == name)
    }

    /// Every child named `name`.
    pub(crate) fn children<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |node| node.name == name)
    }

    /// The first descendant named `name`, depth first.
    pub(crate) fn find(&self, name: &str) -> Option<&Node> {
        self.children.iter().find_map(|node| {
            (node.name == name)
                .then_some(node)
                .or_else(|| node.find(name))
        })
    }

    /// The trimmed text of the first descendant named `name`.
    pub(crate) fn find_text(&self, name: &str) -> Option<&str> {
        self.find(name)
            .map(|node| node.text.trim())
            .filter(|text| !text.is_empty())
    }
}

/// Parse a document into its root element.
pub(crate) fn parse(input: &str) -> Result<Node, String> {
    let mut stack = vec![Node::default()];
    let mut rest = input;
    while !rest.is_empty() {
        let Some(open) = rest.find('<') else {
            push_text(&mut stack, rest);
            break;
        };
        push_text(&mut stack, &rest[..open]);
        rest = &rest[open..];
        if let Some(body) = rest.strip_prefix("<![CDATA[") {
            let end = body.find("]]>").ok_or("unterminated CDATA")?;
            stack.last_mut().expect("root").text.push_str(&body[..end]);
            rest = &body[end + 3..];
        } else if rest.starts_with("<!--") {
            let end = rest.find("-->").ok_or("unterminated comment")?;
            rest = &rest[end + 3..];
        } else if rest.starts_with("<?") || rest.starts_with("<!") {
            let end = rest.find('>').ok_or("unterminated declaration")?;
            rest = &rest[end + 1..];
        } else {
            let end = tag_end(rest).ok_or("unterminated tag")?;
            let tag = &rest[1..end];
            rest = &rest[end + 1..];
            if let Some(closing) = tag.strip_prefix('/') {
                let node = stack.pop().ok_or("unbalanced close")?;
                if node.name != local_name(closing.trim()) || stack.is_empty() {
                    return Err(format!("unexpected </{}>", closing.trim()));
                }
                stack.last_mut().expect("parent").children.push(node);
            } else {
                let self_closing = tag.ends_with('/');
                let tag = tag.trim_end_matches('/').trim();
                let split = tag.find(char::is_whitespace).unwrap_or(tag.len());
                let node = Node {
                    name: local_name(&tag[..split]).to_string(),
                    attrs: attributes(&tag[split..]),
                    ..Node::default()
                };
                if self_closing {
                    stack.last_mut().expect("parent").children.push(node);
                } else {
                    stack.push(node);
                }
            }
        }
    }
    let mut root = stack.pop().ok_or("empty document")?;
    if !stack.is_empty() {
        return Err("unclosed element".into());
    }
    match root.children.len() {
        1 => Ok(root.children.remove(0)),
        _ => Err("expected one root element".into()),
    }
}

/// The `>` that ends the tag opening `rest`, skipping quoted attribute values.
fn tag_end(rest: &str) -> Option<usize> {
    let mut quote = None;
    for (index, char) in rest.char_indices() {
        match (quote, char) {
            (None, '"' | '\'') => quote = Some(char),
            (Some(open), _) if open == char => quote = None,
            (None, '>') => return Some(index),
            _ => {}
        }
    }
    None
}

/// `key="value"` pairs, by local name; namespace declarations are dropped.
fn attributes(mut rest: &str) -> Vec<(String, String)> {
    let mut attrs = Vec::new();
    while let Some(eq) = rest.find('=') {
        let key = rest[..eq].trim();
        let after = rest[eq + 1..].trim_start();
        let Some(quote) = after.chars().next().filter(|c| *c == '"' || *c == '\'') else {
            break;
        };
        let Some(end) = after[1..].find(quote) else {
            break;
        };
        if key != "xmlns" && !key.starts_with("xmlns:") {
            let mut value = Node::default();
            push_text(std::slice::from_mut(&mut value), &after[1..end + 1]);
            attrs.push((local_name(key).to_string(), value.text));
        }
        rest = &after[end + 2..];
    }
    attrs
}

fn local_name(qualified: &str) -> &str {
    qualified.rsplit(':').next().unwrap_or(qualified)
}

fn push_text(stack: &mut [Node], raw: &str) {
    if raw.is_empty() {
        return;
    }
    let node = stack.last_mut().expect("root");
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        node.text.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let decoded = rest.find(';').and_then(|end| {
            let entity = &rest[1..end];
            let char = match entity {
                "lt" => Some('<'),
                "gt" => Some('>'),
                "amp" => Some('&'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                _ => entity
                    .strip_prefix("#x")
                    .or_else(|| entity.strip_prefix("#X"))
                    .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                    .or_else(|| entity.strip_prefix('#').and_then(|dec| dec.parse().ok()))
                    .and_then(char::from_u32),
            }?;
            Some((char, end))
        });
        match decoded {
            Some((char, end)) => {
                node.text.push(char);
                rest = &rest[end + 1..];
            }
            None => {
                node.text.push('&');
                rest = &rest[1..];
            }
        }
    }
    node.text.push_str(rest);
}

/// Escape text for an element body.
pub(crate) fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}
