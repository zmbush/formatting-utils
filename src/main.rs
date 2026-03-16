mod downloadables;
mod gdrive;
mod gui;

use std::{fs::File, path::PathBuf, time::Duration};

use anyhow::{anyhow, Context as _};
use clap::{Args, Parser, Subcommand};
use eframe::egui;

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
    if let Some((column, suffix)) = c.split_once(":") {
        Ok(ImageColumn {
            column: column.to_string(),
            suffix: suffix.to_string(),
        })
    } else {
        Err(anyhow!("Invalid format for image column: {c}"))
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

    filename: PathBuf,
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

    fn subdir<D: AsRef<std::path::Path>>(&self, dir: D) -> Downloader {
        Downloader {
            dir: self.dir.join(dir.as_ref()),

            ..*self
        }
    }
}

fn main() -> Result<(), anyhow::Error> {
    let rt = tokio::runtime::Runtime::new().context("Failed to create tokio runtime")?;
    let _enter = rt.enter();
    std::thread::spawn(move || {
        rt.block_on(async {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
        })
    });

    let cli = Cli::parse();
    println!("{cli:?}");
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
            let (tx, rx) = std::sync::mpsc::channel();

            tokio::spawn(async move {
                let hub = gdrive::drive_hub(None, cli.token_cache).await;
                downloadables::download(download_images, &hub, |curr, max| {
                    let _ = tx.send((curr, max));
                })
                .await
                .expect("downloa dsec");
            });
            while let Ok((curr, max)) = rx.recv() {
                println!("{}%", (curr as f32 / max as f32) * 100.0);
            }
            Ok(())
        }
    }
}
