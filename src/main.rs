use std::{collections::HashMap, fs::File, path::PathBuf};

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Commands,
}

#[derive(Subcommand)]
enum Commands {
    DownloadImages {
        #[arg(short, long)]
        creator_column: String,

        #[arg(short, long)]
        extra_info_column: Option<String>,

        #[arg(short, long)]
        image_column: String,

        #[arg(short, long)]
        prefix: Option<String>,

        #[arg(short, long)]
        suffix: Option<String>,

        filename: String,
        output_dir: String,
    },
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    let cli = Cli::parse();

    match cli.cmd {
        Commands::DownloadImages {
            creator_column,
            extra_info_column,
            image_column,
            prefix,
            suffix,
            filename,
            output_dir,
        } => {
            for row in csv::Reader::from_path(filename)?.deserialize::<HashMap<String, String>>() {
                let row = row?;

                let contrib = &row[&creator_column];
                let url = &row[&image_column];
                let target_fname_prefix = format!(
                    "{prefix}{contrib}{extra}{suffix}",
                    prefix = prefix.as_deref().unwrap_or("ICON - "),
                    extra = extra_info_column
                        .as_ref()
                        .map(|c| format!(" - {}", row[c]))
                        .unwrap_or_default(),
                    suffix = suffix.as_deref().unwrap_or("")
                );

                if let Some((_, id)) =
                    lazy_regex::regex_captures!("https://drive.google.com/file/d/([^/]+).*", url)
                {
                    let dl_url = format!("https://drive.google.com/uc?export=download&id={id}");
                    let response = reqwest::get(dl_url).await?;
                    let (_, filename) = lazy_regex::regex_captures!(
                        r#"filename="?([^"]*)"?"#,
                        response
                            .headers()
                            .get(reqwest::header::CONTENT_DISPOSITION)
                            .and_then(|disposition| disposition.to_str().ok())
                            .unwrap_or("attachment; filename=\"unknown.png\"")
                    )
                    .unwrap_or(("", "unknown.png"));
                    let filename = PathBuf::from(filename);
                    let final_filename = format!(
                        "{output_dir}/{target_fname_prefix}.{}",
                        filename
                            .extension()
                            .and_then(|ext| ext.to_str())
                            .unwrap_or("png")
                    );
                    let mut file = File::create(final_filename)?;
                    let mut content = std::io::Cursor::new(response.bytes().await?);
                    std::io::copy(&mut content, &mut file)?;
                } else {
                    println!("{}'s icon needs a manual download: {}", contrib, url);
                }
            }
        }
    }

    Ok(())
}
