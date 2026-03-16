use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use eframe::egui::{self, RichText, Widget as _};
use google_drive3::yup_oauth2::authenticator_delegate::InstalledFlowDelegate;
use tokio::sync::mpsc;

use crate::{DownloadImages, ImageColumn};

struct GuiFlowDelegate(Arc<Mutex<Option<AuthState>>>);
impl InstalledFlowDelegate for GuiFlowDelegate {
    fn present_user_url<'a>(
        &'a self,
        url: &'a str,
        need_code: bool,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<String, String>> + Send + 'a>>
    {
        Box::pin(display_auth(url, need_code, self.0.clone()))
    }
}

async fn display_auth(
    url: &str,
    need_code: bool,
    auth_state: Arc<Mutex<Option<AuthState>>>,
) -> Result<String, String> {
    let (tx, mut rx) = mpsc::channel(1);
    *auth_state.lock().unwrap() = Some(AuthState {
        url: url.to_string(),
        need_code,
        tx,
    });

    let resp = rx
        .recv()
        .await
        .ok_or_else(|| "Failed to receive code".to_string());
    println!("RECEIVED");
    *auth_state.lock().unwrap() = None;

    resp
}

#[derive(Default, Debug)]
struct MaybeImageColumn {
    column: Option<String>,
    suffix: String,
}

#[derive(Debug)]
struct AuthState {
    url: String,
    need_code: bool,
    tx: mpsc::Sender<String>,
}

#[derive(Default, Debug)]
pub(crate) struct Ui {
    file: Option<csv::Reader<std::fs::File>>,
    filename: Option<std::path::PathBuf>,
    creator_column: Option<String>,
    extra_info_column: Option<String>,
    image_columns: Vec<ImageColumn>,
    new_image_column: MaybeImageColumn,
    token_cache: Option<PathBuf>,

    download_percent: Arc<Mutex<Option<f32>>>,
    auth_state: Arc<Mutex<Option<AuthState>>>,
}

impl Ui {
    pub fn from_token_cache(token_cache: Option<PathBuf>) -> Self {
        Self {
            token_cache,
            ..Default::default()
        }
    }

    fn labeled_column_selector(
        ui: &mut eframe::egui::Ui,
        file: &mut csv::Reader<std::fs::File>,
        label: impl Into<egui::WidgetText>,
        id_salt: impl std::hash::Hash,
        current: &mut Option<String>,
        unselected: &str,
    ) {
        ui.horizontal(|ui| {
            Ui::column_selector(ui, file, id_salt, current, unselected);
            ui.label(label);
        });
    }

    fn column_selector(
        ui: &mut eframe::egui::Ui,
        file: &mut csv::Reader<std::fs::File>,
        id_salt: impl std::hash::Hash,
        current: &mut Option<String>,
        unselected: &str,
    ) {
        egui::ComboBox::from_id_salt(id_salt)
            .selected_text(current.as_deref().unwrap_or(unselected))
            .show_ui(ui, |ui| {
                if let Ok(headers) = file.headers() {
                    ui.selectable_value(current, None, unselected);
                    for header in headers {
                        ui.selectable_value(current, Some(header.to_string()), header);
                    }
                }
            });
    }
}

impl eframe::App for Ui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            {
                let mut state = self.auth_state.lock().unwrap();
                if let Some(auth_state) = &mut *state {
                    ctx.show_viewport_immediate(
                        egui::ViewportId::from_hash_of("authentication window"),
                        egui::ViewportBuilder::default()
                            .with_title("Google Drive Authentication")
                            .with_inner_size(&[200.0, 100.0])
                            .with_resizable(false),
                        |ctx, class| {
                            assert!(
                                class == egui::ViewportClass::Immediate,
                                "This egui backend doesn't support multiple viewports"
                            );

                            egui::CentralPanel::default().show(ctx, |ui| {
                                egui::Label::new(
                                    "The app must authenticate with google to \
                                     support downloading files from google drive.",
                                )
                                .halign(egui::Align::Center)
                                .ui(ui);
                                if ui.button("Click Here To Begin").clicked() {
                                    ctx.open_url(egui::OpenUrl {
                                        url: auth_state.url.clone(),
                                        new_tab: true,
                                    });
                                    tokio::spawn({
                                        let tx = auth_state.tx.clone();
                                        async move {
                                            println!("RUNNING SEND OF TX");
                                            let _ = tx.send(String::new()).await;
                                        }
                                    });
                                }
                            });
                        },
                    );
                }
            }

            let maybe_dl_prog = *self.download_percent.lock().unwrap();
            if let Some(dl_prog) = maybe_dl_prog {
                ui.label("Download in progress...");
                egui::ProgressBar::new(dl_prog)
                    .animate(true)
                    .show_percentage()
                    .ui(ui);
            } else {
                match &mut self.file {
                    None => {
                        ui.label("Open a CSV export with the information to download...");

                        if ui.button("Open file...").clicked() {
                            if let Some(path) = rfd::FileDialog::new()
                                .add_filter("CSV Files", &["csv"])
                                .pick_file()
                            {
                                self.file = csv::Reader::from_path(&path).ok();
                                self.filename = Some(path);
                            }
                        }
                    }
                    Some(file) => {
                        ui.label("Basic Information");
                        Ui::labeled_column_selector(
                            ui,
                            file,
                            "Creator Column (required)",
                            "creator-column",
                            &mut self.creator_column,
                            "- creator -",
                        );
                        Ui::labeled_column_selector(
                            ui,
                            file,
                            "Extra Info",
                            "extra-info",
                            &mut self.extra_info_column,
                            "- extra info -",
                        );

                        ui.separator();
                        ui.label("Image Columns (Must have at least one)");

                        let mut image_columns = Vec::new();
                        std::mem::swap(&mut image_columns, &mut self.image_columns);
                        self.image_columns = image_columns
                            .into_iter()
                            .enumerate()
                            .filter_map(|(i, mut image_column)| {
                                ui.horizontal(|ui| {
                                    egui::ComboBox::from_id_salt(format!("image-column-{i}"))
                                        .selected_text(&image_column.column)
                                        .show_ui(ui, |ui| {
                                            if let Ok(headers) = file.headers() {
                                                for header in headers {
                                                    ui.selectable_value(
                                                        &mut image_column.column,
                                                        header.to_string(),
                                                        header,
                                                    );
                                                }
                                            }
                                        });
                                    ui.add_enabled(
                                        false,
                                        egui::TextEdit::singleline(&mut image_column.suffix),
                                    );
                                    if ui.button("-").clicked() {
                                        None
                                    } else {
                                        Some(image_column)
                                    }
                                })
                                .inner
                            })
                            .collect();

                        ui.horizontal(|ui| {
                            Ui::column_selector(
                                ui,
                                file,
                                "new-image-column",
                                &mut self.new_image_column.column,
                                "- column -",
                            );
                            ui.text_edit_singleline(&mut self.new_image_column.suffix);
                            if ui
                                .add_enabled(
                                    self.new_image_column.column.is_some()
                                        && !self.new_image_column.suffix.is_empty(),
                                    egui::Button::new("+"),
                                )
                                .clicked()
                            {
                                let mut new_image_column = Default::default();
                                std::mem::swap(&mut new_image_column, &mut self.new_image_column);
                                self.image_columns.push(ImageColumn {
                                    column: new_image_column.column.unwrap(),
                                    suffix: new_image_column.suffix,
                                });
                            }
                        });

                        ui.separator();

                        if ui
                            .add_enabled(
                                self.creator_column.is_some() && !self.image_columns.is_empty(),
                                egui::Button::new("Download Images"),
                            )
                            .clicked()
                        {
                            let token_cache = self.token_cache.clone();
                            let mut data = Ui::default();
                            std::mem::swap(self, &mut data);
                            let ctx = (*ctx).clone();
                            let download_percent = self.download_percent.clone();
                            let auth_state = self.auth_state.clone();
                            tokio::spawn(async move {
                                let hub = crate::gdrive::drive_hub(
                                    Some(Box::new(GuiFlowDelegate(auth_state))),
                                    token_cache,
                                )
                                .await;

                                crate::downloadables::download(
                                    DownloadImages {
                                        creator_column: data.creator_column.unwrap(),
                                        extra_info_column: data.extra_info_column,
                                        image_column: data.image_columns,
                                        prefix: None,
                                        suffix: None,
                                        filename: data.filename.unwrap(),
                                    },
                                    &hub,
                                    move |curr, max| {
                                        if curr < max {
                                            *download_percent.lock().unwrap() =
                                                Some(curr as f32 / max as f32);
                                        } else {
                                            *download_percent.lock().unwrap() = None;
                                        }
                                        ctx.request_repaint();
                                    },
                                )
                                .await
                            });
                        }
                    }
                }
            }
        });
    }
}
