#![deny(unused)]

mod downloadables;
mod gdrive;
mod gui;

use std::{
    fs::File,
    io::{Cursor, Write as _},
    path::PathBuf,
    process::{Command, Stdio},
};

use anyhow::{anyhow, Context as _};
use bytes::Bytes;
use clap::{Args, Parser, Subcommand};
use eframe::egui;
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::Url;

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Commands>,

    #[arg(long)]
    token_cache: Option<PathBuf>,
}

#[derive(Clone, Debug)]
struct ImageColumn {
    column: String,
    suffix: String,
}

fn parse_image_column(c: &str) -> Result<ImageColumn, anyhow::Error> {
    if let Some((column, suffix)) = c.rsplit_once(":") {
        Ok(ImageColumn {
            column: column.to_string(),
            suffix: suffix.to_string(),
        })
    } else {
        Err(anyhow!("Invalid format for image column: {c}"))
    }
}

#[derive(Debug, Clone)]
enum FileInput {
    Local(PathBuf),
    Remote(Url),
}

impl std::str::FromStr for FileInput {
    type Err = !;

    fn from_str(s: &str) -> Result<Self, !> {
        if let Ok(url) = Url::parse(s) {
            if url.scheme().len() > 1 {
                return Ok(FileInput::Remote(url));
            }
        }
        Ok(FileInput::Local(PathBuf::from(s)))
    }
}

enum InputReader {
    Local(File),
    Remote(Cursor<Bytes>),
}

impl std::io::Read for InputReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            InputReader::Local(file) => file.read(buf),
            InputReader::Remote(bytes) => bytes.read(buf),
        }
    }
}

impl FileInput {
    pub fn name(&self) -> String {
        match self {
            FileInput::Local(path_buf) => path_buf
                .file_stem()
                .expect("no file stem")
                .to_string_lossy()
                .to_string(),
            FileInput::Remote(url) => {
                let id = url
                    .path()
                    .strip_prefix("/spreadsheets/d/")
                    .expect("not a spreadsheet url")
                    .split('/')
                    .next()
                    .expect("there should always be one");
                id.to_string()
            }
        }
    }

    async fn csv_reader(&self) -> Result<csv::Reader<InputReader>, anyhow::Error> {
        Ok(match self {
            FileInput::Local(path_buf) => {
                csv::ReaderBuilder::default().from_reader(InputReader::Local(File::open(path_buf)?))
            }
            FileInput::Remote(url) => {
                let id = url
                    .path()
                    .strip_prefix("/spreadsheets/d/")
                    .ok_or_else(|| anyhow!("Not a spreadsheets url"))?
                    .split('/')
                    .next()
                    .expect("there should always be one");
                let url =
                    format!("https://docs.google.com/spreadsheets/d/{id}/gviz/tq?tqx=out:csv");
                let response = reqwest::get(url).await?;
                csv::ReaderBuilder::default()
                    .from_reader(InputReader::Remote(Cursor::new(response.bytes().await?)))
            }
        })
    }
}

#[derive(Args, Debug)]
struct DownloadImages {
    #[arg(short, long)]
    creator_column: String,

    #[arg(short, long)]
    extra_info_column: Option<String>,

    #[arg(short, long, value_parser = parse_image_column)]
    image_column: Vec<ImageColumn>,

    #[arg(short, long)]
    prefix: Option<String>,

    #[arg(short, long)]
    suffix: Option<String>,

    file: FileInput,
}

#[derive(Subcommand, Debug)]
enum Commands {
    DownloadImages(DownloadImages),
}

#[derive(Copy, Clone)]
struct DownloaderContext<'a> {
    global_prefix: &'a str,
    global_suffix: &'a str,
}

struct Downloader<'a> {
    dir: PathBuf,
    ctx: DownloaderContext<'a>,
    extra_info: &'a str,
    contrib: &'a str,
    suffix: &'a str,
}

impl Downloader<'_> {
    fn convert_and_save<F, D>(
        &self,
        file_name: F,
        data: D,
        from: &str,
        to: &str,
    ) -> Result<(), anyhow::Error>
    where
        F: AsRef<std::ffi::OsStr>,
        D: AsRef<[u8]>,
    {
        let mut cmd = Command::new("pandoc")
            .args(["-s", "-f", from, "-t", to])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .context("Couldn't start pandoc")?;

        cmd.stdin
            .as_mut()
            .expect("Can't get stdin pipe")
            .write_all(data.as_ref())
            .context("Couldn't write to pandoc stdin")?;
        let output = cmd
            .wait_with_output()
            .context("while getting pandoc output")?;

        let data = output.stdout;
        self.save(file_name, data)
    }

    fn save<F, D>(&self, file_name: F, data: D) -> Result<(), anyhow::Error>
    where
        F: AsRef<std::ffi::OsStr>,
        D: AsRef<[u8]>,
    {
        std::fs::create_dir_all(&self.dir).with_context(|| {
            format!("Could not create containing folder {}", self.dir.display())
        })?;
        let final_filename = self.dir.join(file_name.as_ref());
        let mut file = File::create(&final_filename)
            .with_context(|| format!("While opening {}", final_filename.display()))?;
        let mut content = std::io::Cursor::new(data);
        std::io::copy(&mut content, &mut file)?;
        Ok(())
    }

    fn base_name(&self) -> String {
        format!(
            "{prefix}{contrib}{extra} - {column_suffix}{global_suffix}",
            prefix = self.ctx.global_prefix,
            contrib = self.contrib,
            column_suffix = self.suffix,
            extra = self.extra_info,
            global_suffix = self.ctx.global_suffix,
        )
        .replace(['/', '\\'], "_")
    }

    fn file_name<F: AsRef<std::path::Path>>(&self, filename: F) -> String {
        format!(
            "{}.{}",
            self.base_name(),
            filename
                .as_ref()
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("png")
        )
    }

    fn subdir<D: AsRef<std::path::Path>>(&'_ self, dir: D) -> Downloader<'_> {
        Downloader {
            dir: self.dir.join(dir.as_ref()),

            ..*self
        }
    }
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();
    match cli.cmd {
        None => {
            // GUI!
            let options = eframe::NativeOptions {
                viewport: egui::ViewportBuilder::default().with_inner_size([640.0, 240.0]),
                ..Default::default()
            };
            Ok(eframe::run_native(
                "Formatting Utils",
                options,
                Box::new(|_cc| Ok(Box::new(gui::Ui::from_token_cache(cli.token_cache)))),
            )
            .map_err(|e| anyhow::anyhow!("E: {e:?}"))?)
        }
        Some(Commands::DownloadImages(download_images)) => {
            let bar = ProgressBar::new(10_000).with_style(
                ProgressStyle::with_template(
                    "[{elapsed_precise}]{msg} {wide_bar:.cyan/blue} {percent}%",
                )
                .expect("bad ProgressStyle"),
            );
            let (tx, rx) = std::sync::mpsc::channel();

            let dl = tokio::spawn(async move {
                let hub = gdrive::drive_hub(None, cli.token_cache).await;
                let tx2 = tx.clone();
                let fetch_failures = downloadables::download(download_images, &hub, |status| {
                    let _ = tx.send(Some(status.clone()));
                })
                .await
                .expect("failed to download images");
                let _ = tx2.send(None);

                fetch_failures
            });

            while let Ok(status) = rx.recv() {
                if let Some(status) = status {
                    bar.set_position(
                        ((status.done as f32 / status.total as f32) * 10_000.0) as u64,
                    );
                    let mut msg = String::new();
                    if let Some(creator) = status.creator {
                        msg += &creator;
                    }
                    if let Some(column) = status.column {
                        msg += &format!("({column})");
                    }
                    if !msg.is_empty() {
                        msg = format!(" {msg}");
                    }
                    bar.set_message(msg);
                } else {
                    bar.finish_with_message(" Download Complete");
                    break;
                }
            }
            let fetch_failures = dl.await?;
            if !fetch_failures.is_empty() {
                println!("\n\nFetch Failures:")
            }
            for downloadables::FetchFailure { filename, url } in fetch_failures {
                println!("{filename}: {url}",);
            }
            Ok(())
        }
    }
}
