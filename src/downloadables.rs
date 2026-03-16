use super::Downloader;
use crate::{gdrive, DownloadImages, DownloaderContext};
use anyhow::Context as _;
use google_drive3::{
    common::Connector, hyper_rustls::HttpsConnector,
    hyper_util::client::legacy::connect::HttpConnector,
};
use http_body_util::BodyExt as _;
use std::{collections::HashMap, future::Future, path::PathBuf, pin::Pin};

pub(crate) async fn download(
    DownloadImages {
        creator_column,
        extra_info_column,
        image_column,
        prefix,
        suffix,
        filename,
    }: DownloadImages,
    drive_hub: &gdrive::Hub,
    mut status: impl FnMut(usize, usize),
) -> Result<(), anyhow::Error> {
    let global_prefix = prefix
        .as_deref()
        .map(|c| format!("{} - ", c))
        .unwrap_or_else(String::new);
    let global_suffix = suffix
        .as_deref()
        .map(|c| format!(" - {}", c))
        .unwrap_or_else(String::new);
    let download_ctx = DownloaderContext {
        global_prefix: &global_prefix,
        global_suffix: &global_suffix,
    };

    let mut downloadables = Downloadables::<HttpsConnector<HttpConnector>>::default();
    downloadables.register(DriveFile(drive_hub));
    downloadables.register(DriveDirectory::new(drive_hub));
    downloadables.register(DriveDocument(drive_hub));
    downloadables.register(Dropbox);
    downloadables.register(GenericFile);

    let mut output_dir = PathBuf::new();
    output_dir.push("outputs");
    output_dir.push(filename.file_stem().unwrap());

    let file = csv::Reader::from_path(&filename)?
        .deserialize::<HashMap<String, String>>()
        .collect::<Vec<_>>();
    let max = file.len();

    status(0, max);
    for (i, row) in file.into_iter().enumerate() {
        status(i + 1, max);

        let row = row?;

        for image_column in &image_column {
            let contrib = &row[&creator_column];
            let url = &row[&image_column.column];

            let extra_info = extra_info_column
                .as_ref()
                .map(|c| format!(" - {}", row[c]))
                .unwrap_or_default();
            let downloader = Downloader {
                dir: PathBuf::from(&output_dir),
                ctx: download_ctx,
                extra_info: &extra_info,
                contrib,
                suffix: &image_column.suffix,
            };

            if url.is_empty() {
                continue;
            }
            if !downloadables.download(url, &downloader).await {
                println!(
                    "{contrib}'s file in column `{}` needs a manual download: {url}",
                    image_column.column,
                );
            }
        }
    }

    Ok(())
}

pub(crate) trait Downloadable<C: Connector>: Send + Sync {
    fn matches(&self, url: &str) -> Option<String>;

    fn download<'a>(
        &'a self,
        id: String,
        downloader: &'a Downloader<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), anyhow::Error>> + Send + 'a>>;
}

pub(crate) struct Downloadables<'a, C: Connector>(Vec<Box<dyn Downloadable<C> + 'a>>);

impl<C: Connector> Default for Downloadables<'_, C> {
    fn default() -> Self {
        Self(Default::default())
    }
}

impl<'a, C: Connector> Downloadables<'a, C> {
    pub(crate) fn register<D: Downloadable<C> + 'a>(&mut self, d: D) {
        self.0.push(Box::new(d))
    }

    pub(crate) async fn download<'b>(&'b self, url: &str, downloader: &'b Downloader<'b>) -> bool {
        for dl in &self.0 {
            if let Some(id) = dl.matches(url) {
                if let Err(e) = dl.download(id, downloader).await {
                    println!("Failed to download: {e:?}");
                } else {
                    return true;
                }
            }
        }
        false
    }
}

pub(crate) struct DriveFile<'h>(&'h gdrive::Hub);

impl<C: Connector> Downloadable<C> for DriveFile<'_> {
    fn matches(&self, url: &str) -> Option<String> {
        Some(
            lazy_regex::regex_captures!("https://drive.google.com/file/d/([^/]+).*", url)
                .or_else(|| {
                    lazy_regex::regex_captures!("https://drive.google.com/open\\?id=([^/]+).*", url)
                })?
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
            let (_, file_info) = self
                .0
                .files()
                .get(&id)
                .add_scope(google_drive3::api::Scope::Readonly)
                .doit()
                .await
                .context("While fetching file info")?;

            let (file_contents, _) = self
                .0
                .files()
                .get(&id)
                .add_scope(google_drive3::api::Scope::Readonly)
                .acknowledge_abuse(true)
                .param("alt", "media")
                .doit()
                .await
                .context("While fetching file")?;

            let final_filename =
                downloader.file_name(file_info.name.unwrap_or_else(|| "unknown.png".into()));

            downloader.save(
                final_filename,
                file_contents
                    .into_body()
                    .collect()
                    .await
                    .map(|value| value.to_bytes())?,
            )
        })
    }
}

pub(crate) struct DriveDirectory<'h> {
    hub: &'h gdrive::Hub,
    nested: bool,
}

impl DriveDirectory<'_> {
    fn new(hub: &gdrive::Hub) -> DriveDirectory {
        DriveDirectory { hub, nested: false }
    }
}

impl<C: Connector> Downloadable<C> for DriveDirectory<'_> {
    fn matches(&self, url: &str) -> Option<String> {
        Some(
            lazy_regex::regex_captures!("https://drive.google.com/drive/folders/([^/?]+).*", url)?
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
            let q = format!("'{id}' in parents");
            let (_, files) = self
                .hub
                .files()
                .list()
                .add_scope(google_drive3::api::Scope::Readonly)
                .q(&q)
                .doit()
                .await?;
            let dir = downloader.dir.join(downloader.base_name());

            let root = downloader.subdir(downloader.base_name());
            let downloader = if !self.nested {
                std::fs::create_dir_all(&dir).with_context(|| {
                    format!("While creating directory for {}", downloader.contrib)
                })?;
                &root
            } else {
                downloader
            };

            if let Some(files) = files.files {
                for file in files {
                    let id = file.id.unwrap_or_default();
                    let filename = file.name.unwrap_or_else(|| "unknown.png".into());

                    match file.mime_type.as_deref() {
                        Some("application/vnd.google-apps.folder") => {
                            let downloader = downloader.subdir(filename);
                            <DriveDirectory as Downloadable<C>>::download(
                                &DriveDirectory {
                                    hub: self.hub,
                                    nested: true,
                                },
                                id.clone(),
                                &downloader,
                            )
                            .await?
                        }
                        _ => {
                            let (response, _) = self
                                .hub
                                .files()
                                .get(&id)
                                .add_scope(google_drive3::api::Scope::Readonly)
                                .acknowledge_abuse(true)
                                .param("alt", "media")
                                .doit()
                                .await?;

                            let bytes = response
                                .into_body()
                                .collect()
                                .await
                                .map(|value| value.to_bytes())
                                .expect("");

                            downloader.save(filename, bytes)?;
                        }
                    }
                }
            }

            Ok(())
        })
    }
}

pub(crate) struct DriveDocument<'h>(&'h gdrive::Hub);

impl<C: Connector> Downloadable<C> for DriveDocument<'_> {
    fn matches(&self, url: &str) -> Option<String> {
        Some(
            lazy_regex::regex_captures!("https://docs.google.com/document/d/([^/?]+).*", url)?
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
            let response = self
                .0
                .files()
                .export(&id, "application/rtf")
                .add_scope(google_drive3::api::Scope::Readonly)
                .doit()
                .await?;

            let final_filename = downloader.file_name("file.rtf");

            downloader.save(
                final_filename,
                response
                    .into_body()
                    .collect()
                    .await
                    .map(|value| value.to_bytes())?,
            )
        })
    }
}

pub(crate) struct Dropbox;

impl<C: Connector> Downloadable<C> for Dropbox {
    fn matches(&self, url: &str) -> Option<String> {
        Some(
            lazy_regex::regex_captures!("https://www.dropbox.com/(.*)&dl=0", url)?
                .1
                .to_string(),
        )
    }

    fn download<'a>(
        &self,
        prefix: String,
        downloader: &'a Downloader<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), anyhow::Error>> + Send + 'a>> {
        Box::pin(async move {
            let dl_url = format!("https://dl.dropbox.com/{prefix}&dl=1");
            let response = reqwest::get(dl_url).await?;
            let filename = lazy_regex::regex_captures!(
                r#"filename="?([^"]*)"?"#,
                response
                    .headers()
                    .get(reqwest::header::CONTENT_DISPOSITION)
                    .and_then(|disposition| disposition.to_str().ok())
                    .unwrap_or("attachment; filename=\"unknown.png\"")
            )
            .unwrap_or(("", "unknown.png"))
            .1
            .to_string();

            let final_filename = downloader.file_name(filename);

            downloader.save(final_filename, response.bytes().await?)
        })
    }
}

pub(crate) struct GenericFile;
impl<C: Connector> Downloadable<C> for GenericFile {
    fn matches(&self, url: &str) -> Option<String> {
        if url.ends_with(".jpg")
            || url.ends_with(".png")
            || url.ends_with(".gif")
            || url.ends_with(".pdf")
            || url.ends_with(".tif")
        {
            Some(url.to_string())
        } else {
            None
        }
    }

    fn download<'a>(
        &self,
        url: String,
        downloader: &'a Downloader<'a>,
    ) -> Pin<Box<dyn Future<Output = Result<(), anyhow::Error>> + Send + 'a>> {
        Box::pin(async move {
            let final_filename = downloader.file_name(&url);
            downloader.save(final_filename, reqwest::get(&url).await?.bytes().await?)
        })
    }
}
