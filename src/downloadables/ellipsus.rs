use std::{borrow::Cow, future::Future, pin::Pin};

use anyhow::Context as _;
use base64::prelude::*;
use google_drive3::common::Connector;
use reqwest::{header, Client};
use serde::{Deserialize, Serialize};
use yrs::{
    updates::decoder::Decode as _, Array as _, GetString as _, Out, ReadTxn, Transact, Xml as _,
    XmlElementRef, XmlFragment as _,
};

use crate::{downloadables::Downloadable, Downloader};

pub(crate) struct Ellipsus;
impl<C: Connector> Downloadable<C> for Ellipsus {
    fn matches(&self, url: &str) -> Option<String> {
        Some(
            lazy_regex::regex_captures!("https://ellipsus.com/read/([^/]*)", url)?
                .1
                .to_string(),
        )
    }

    fn download<'a>(
        &'a self,
        id: String,
        downloader: &'a Downloader<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), anyhow::Error>> + Send + 'a>> {
        Box::pin(async move {
            let final_filename = downloader.file_name("doc.rtf");
            let doc = EllipsusDoc::from_id(&id)
                .await
                .context("Failed to get ellipsus doc")?;
            let html = doc.contents().context("Failed to generate content")?;
            downloader.convert_and_save(final_filename, html, "html", "rtf")
        })
    }
}

struct EllipsusDoc(yrs::Doc);

impl EllipsusDoc {
    async fn from_id(id: &str) -> Result<Self, anyhow::Error> {
        // Try to convince ellipsus we're a real web browser.
        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::USER_AGENT,
            header::HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                     (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36",
            ),
        );
        headers.insert(
            header::ACCEPT,
            header::HeaderValue::from_static(
                "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8",
            ),
        );

        let client = Client::builder()
            .http1_only()
            .default_headers(headers)
            .build()?;
        let html = client
            .get(format!("https://ellipsus.com/read/{id}"))
            .send()
            .await?
            .text()
            .await?;

        EllipsusDoc::from_ellipsus_html(&html)
    }

    fn from_ellipsus_html(html: &str) -> Result<Self, anyhow::Error> {
        let document = scraper::Html::parse_document(html);
        let selector = scraper::Selector::parse("script#__NEXT_DATA__").expect("static selector");

        if let Some(element) = document.select(&selector).next() {
            let json: serde_json::Value = serde_json::from_str(element.text().next().unwrap())?;
            return Self::from_react_hydration(json);
        }

        Err(anyhow::anyhow!("unable to find __NEXT_DATA__"))
    }

    fn from_react_hydration(value: serde_json::Value) -> Result<Self, anyhow::Error> {
        for query in value["props"]["pageProps"]["dehydratedState"]["queries"]
            .as_array()
            .unwrap()
        {
            if let Some(str) = query["state"]["data"]["content"].as_str() {
                return Self::from_base64(str);
            }
        }

        Err(anyhow::anyhow!("couldn't find rehydration parameters"))
    }

    fn from_base64(data: &str) -> Result<Self, anyhow::Error> {
        let bytes: Vec<u8> = BASE64_STANDARD.decode(data).context("Decoding base64")?;
        let doc = yrs::Doc::new();

        {
            let mut txn = doc.transact_mut();
            let update = yrs::Update::decode_v1(&bytes).context("decoding yjs updates")?;
            txn.apply_update(update).context("applying yjs updates")?;
        }

        Ok(EllipsusDoc(doc))
    }

    fn contents(&self) -> Result<String, anyhow::Error> {
        let draft_content = self.0.get_or_insert_array("draft-content");
        let mut contents = String::new();
        contents += "<!html>\n<body>\n";

        let txn = self.0.transact();
        for paragraph in draft_content.iter(&txn) {
            if let Out::YXmlElement(xr) = paragraph {
                contents += &yxml_element_to_dom(&txn, &xr)
                    .context("While making dom")
                    .expect("Failed to construct ellipsus dom. Maybe add more variants to `Node`?");
            }
        }

        contents += "</body>";

        Ok(contents)
    }
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct Document {
    #[serde(rename = "$value", default)]
    nodes: Vec<Node>,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
enum Node {
    #[serde(rename = "heading")]
    Heading {
        #[serde(rename = "@level")]
        level: String,
        #[serde(rename = "$value", default)]
        content: Vec<Node>,
        #[serde(rename = "@textAlign")]
        text_align: Option<String>,
        #[serde(rename = "@indentDirection")]
        indent_direction: Option<String>,
        #[serde(rename = "@indent")]
        indent: Option<String>,
    },

    #[serde(rename = "paragraph")]
    Paragraph {
        #[serde(rename = "$value", default)]
        content: Vec<Node>,
        #[serde(rename = "@textAlign")]
        text_align: Option<String>,
        #[serde(rename = "@indentDirection")]
        indent_direction: Option<String>,
        #[serde(rename = "@indent")]
        indent: Option<String>,
    },

    #[serde(rename = "bold")]
    Bold {
        #[serde(rename = "$value", default)]
        content: Vec<Node>,
    },

    #[serde(rename = "italic")]
    Italic {
        #[serde(rename = "$value", default)]
        content: Vec<Node>,
    },

    #[serde(rename = "link")]
    Link {
        #[serde(rename = "@href")]
        href: String,
        #[serde(rename = "@rel")]
        rel: Option<String>,
        #[serde(rename = "@target")]
        target: Option<String>,
        #[serde(rename = "$value", default)]
        content: Vec<Node>,
    },

    #[serde(rename = "hardBreak")]
    Br,

    #[serde(rename = "horizontalRule")]
    Hr,

    #[serde(rename = "bulletList")]
    BulletList {
        #[serde(rename = "$value", default)]
        content: Vec<Node>,
        #[serde(rename = "@textAlign")]
        text_align: Option<String>,
        #[serde(rename = "@indentDirection")]
        indent_direction: Option<String>,
        #[serde(rename = "@indent")]
        indent: Option<String>,
    },

    #[serde(rename = "orderedList")]
    OrderedList {
        #[serde(rename = "$value", default)]
        content: Vec<Node>,
        #[serde(rename = "@start")]
        start: Option<String>,
        #[serde(rename = "@textAlign")]
        text_align: Option<String>,
        #[serde(rename = "@indentDirection")]
        indent_direction: Option<String>,
        #[serde(rename = "@indent")]
        indent: Option<String>,
    },

    #[serde(rename = "listItem")]
    ListItem {
        #[serde(rename = "$value", default)]
        content: Vec<Node>,
    },

    #[serde(rename = "highlight")]
    Highlight {
        #[serde(rename = "@color")]
        color: String,

        #[serde(rename = "$value", default)]
        content: Vec<Node>,
    },

    #[serde(rename = "$text")]
    Text(String),
}

impl Node {
    fn tag(&self) -> Option<Cow<'_, str>> {
        Some(match self {
            Node::Heading { level, .. } => format!("h{level}").into(),
            Node::Paragraph { .. } => "p".into(),
            Node::Bold { .. } => "b".into(),
            Node::Italic { .. } => "i".into(),
            Node::Link { .. } => "a".into(),
            Node::Br => "br".into(),
            Node::Hr => "hr".into(),
            Node::BulletList { .. } => "ul".into(),
            Node::OrderedList { .. } => "ol".into(),
            Node::ListItem { .. } => "li".into(),
            Node::Highlight { .. } => "span".into(),
            Node::Text(_) => return None,
        })
    }

    fn attrs(&self) -> String {
        let parts = match self {
            Node::Heading {
                level: _,
                content: _,
                text_align,
                indent_direction,
                indent,
            } => {
                let mut parts = Vec::new();
                if let Some(align) = text_align {
                    parts.push(format!("textAlign=\"{align}\""));
                }
                if let Some(dir) = indent_direction {
                    parts.push(format!("indentDirection=\"{dir}\""))
                }
                if let Some(indent) = indent {
                    parts.push(format!("indent=\"{indent}\""))
                }
                parts
            }
            Node::Paragraph {
                content: _,
                text_align,
                indent_direction,
                indent,
            } => {
                let mut parts = Vec::new();
                if let Some(align) = text_align {
                    parts.push(format!("textAlign=\"{align}\""));
                }
                if let Some(dir) = indent_direction {
                    parts.push(format!("indentDirection=\"{dir}\""))
                }
                if let Some(indent) = indent {
                    parts.push(format!("indent=\"{indent}\""))
                }
                parts
            }
            Node::Bold { .. } => Vec::new(),
            Node::Italic { .. } => Vec::new(),
            Node::Link {
                href,
                rel,
                target,
                content: _,
            } => {
                let mut parts = Vec::new();
                parts.push(format!("href=\"{href}\""));
                if let Some(rel) = rel {
                    parts.push(format!("rel=\"{rel}\""))
                }
                if let Some(target) = target {
                    parts.push(format!("indent=\"{target}\""))
                }
                parts
            }
            Node::Br => Vec::new(),
            Node::Hr => Vec::new(),
            Node::BulletList {
                content: _,
                text_align,
                indent_direction,
                indent,
            } => {
                let mut parts = Vec::new();
                if let Some(align) = text_align {
                    parts.push(format!("textAlign=\"{align}\""));
                }
                if let Some(dir) = indent_direction {
                    parts.push(format!("indentDirection=\"{dir}\""))
                }
                if let Some(indent) = indent {
                    parts.push(format!("indent=\"{indent}\""))
                }
                parts
            }
            Node::OrderedList {
                content: _,
                start,
                text_align,
                indent_direction,
                indent,
            } => {
                let mut parts = Vec::new();
                if let Some(start) = start {
                    parts.push(format!("start=\"{start}\""));
                }
                if let Some(align) = text_align {
                    parts.push(format!("textAlign=\"{align}\""));
                }
                if let Some(dir) = indent_direction {
                    parts.push(format!("indentDirection=\"{dir}\""))
                }
                if let Some(indent) = indent {
                    parts.push(format!("indent=\"{indent}\""))
                }
                parts
            }
            Node::ListItem { .. } => Vec::new(),
            Node::Highlight { color, .. } => {
                vec![format!(
                    "style=\"background-color: {};\"",
                    color.strip_prefix("--color-").unwrap_or(color)
                )]
            }
            Node::Text(_) => Vec::new(),
        };

        parts.join(" ")
    }

    fn content(&self) -> &[Node] {
        match self {
            Node::Heading { content, .. } => content,
            Node::Paragraph { content, .. } => content,
            Node::Bold { content } => content,
            Node::Italic { content } => content,
            Node::Link { content, .. } => content,
            Node::Br => &[],
            Node::Hr => &[],
            Node::BulletList { content, .. } => content,
            Node::OrderedList { content, .. } => content,
            Node::ListItem { content } => content,
            Node::Highlight { content, .. } => content,
            Node::Text(_) => &[],
        }
    }

    fn self_closing(&self) -> bool {
        matches!(self, Node::Br | Node::Hr)
    }

    fn text(&self) -> Option<&str> {
        match self {
            Node::Text(text) => Some(text),
            _ => None,
        }
    }
}

fn yxml_element_to_dom<T: ReadTxn>(
    txn: &T,
    y_elem: &XmlElementRef,
) -> Result<String, anyhow::Error> {
    let xml = yxml_element_to_xml(txn, y_elem)?;
    let doc: Document = quick_xml::de::from_str(&format!("<doc>{xml}</doc>"))?;

    fn doc_to_str(nodes: &[Node], indent: &str) -> String {
        let mut output = String::new();
        for node in nodes {
            let Some(tag) = node.tag() else {
                output += &format!("{indent}{}\n", node.text().expect("no tag must mean text"));
                continue;
            };
            if node.self_closing() {
                output += &format!("{indent}<{} />\n", tag);
                continue;
            }

            output += &format!("{indent}<{tag} {}>\n", node.attrs());
            output += &doc_to_str(node.content(), &format!("{indent}  "));
            output += &format!("{indent}</{tag}>\n");
        }
        output
    }

    Ok(doc_to_str(&doc.nodes, " "))
}

fn yxml_element_to_xml<T: ReadTxn>(
    txn: &T,
    y_elem: &XmlElementRef,
) -> Result<String, anyhow::Error> {
    let mut node = String::new();
    let tag = y_elem.tag().as_ref();
    node += "<";
    node += tag;
    for (key, value) in y_elem.attributes(txn) {
        node += &format!(" {key}=\"{value}\"");
    }
    node += ">";
    for child in y_elem.children(txn) {
        node += &match child {
            yrs::XmlOut::Element(xml_element_ref) => yxml_element_to_xml(txn, &xml_element_ref)?,
            yrs::XmlOut::Fragment(_xml_fragment_ref) => todo!(),
            yrs::XmlOut::Text(xml_text_ref) => xml_text_ref.get_string(txn),
        };
    }
    node += &format!("</{tag}>\n");

    Ok(node)
}

#[cfg(test)]
mod tests {
    use crate::downloadables::ellipsus::EllipsusDoc;

    #[tokio::test]
    async fn test_fetch() {
        let doc = EllipsusDoc::from_id("5G0wzFS8rXpfZYfruFvodP")
            .await
            .unwrap();
        println!("{}", doc.contents().unwrap());
        assert_eq!("doc", doc.contents().unwrap());
    }
}
