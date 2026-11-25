use std::{future::Future, pin::Pin};

use anyhow::Context as _;
use base64::prelude::*;
use google_drive3::common::Connector;
use reqwest::{header, Client};
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
                contents += &yxml_element_to_dom(&txn, &xr).context("While making dom")?;
            }
        }

        contents += "</body>";

        Ok(contents)
    }
}

fn yxml_element_to_dom<T: ReadTxn>(
    txn: &T,
    y_elem: &XmlElementRef,
) -> Result<String, anyhow::Error> {
    let mut node = String::new();
    let tag = match y_elem.tag().as_ref() {
        "paragraph" => "p",
        "horizontalRule" => "hr",
        "italic" => "i",
        "bold" => "b",
        "highlight" => "span",
        tag => tag,
    };
    node += "<";
    node += tag;
    for (key, value) in y_elem.attributes(txn) {
        node += &format!(" {key}=\"{value}\"");
    }
    if tag == "hr" {
        node += " />\n";
        return Ok(node);
    }
    node += ">";
    for child in y_elem.children(txn) {
        node += &match child {
            yrs::XmlOut::Element(xml_element_ref) => yxml_element_to_dom(txn, &xml_element_ref)?,
            yrs::XmlOut::Fragment(_xml_fragment_ref) => todo!(),
            yrs::XmlOut::Text(xml_text_ref) => xml_text_ref
                .get_string(txn)
                .replace("<highlight", "<span")
                .replace("<italic", "<i")
                .replace("<bold", "<b")
                .replace("</highlight", "</span")
                .replace("</italic", "</i")
                .replace("</bold", "</b"),
        };
    }
    node += &format!("</{tag}>\n");

    Ok(node)
}
