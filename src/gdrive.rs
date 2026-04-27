use google_drive3::{
    hyper_rustls::{self, HttpsConnector},
    hyper_util::{self, client::legacy::connect::HttpConnector},
    yup_oauth2::{self},
    DriveHub,
};

pub(crate) type Hub = DriveHub<HttpsConnector<HttpConnector>>;
pub(crate) async fn drive_hub(
    flow_delegate: Option<Box<dyn yup_oauth2::authenticator_delegate::InstalledFlowDelegate>>,
    token_cache: Option<std::path::PathBuf>,
) -> Hub {
    println!("Opening drive hub with token cache: {token_cache:?}");
    let secret = yup_oauth2::read_application_secret("clientsecret.json")
        .await
        .expect("clientsecret.json");
    let mut builder = yup_oauth2::InstalledFlowAuthenticator::builder(
        secret,
        if flow_delegate.is_some() {
            yup_oauth2::InstalledFlowReturnMethod::Interactive
        } else {
            yup_oauth2::InstalledFlowReturnMethod::HTTPRedirect
        },
    )
    .persist_tokens_to_disk(token_cache.unwrap_or_else(|| "tokencache.json".into()));
    if let Some(delegate) = flow_delegate {
        builder = builder.flow_delegate(delegate);
    }
    let auth = builder.build().await.expect("InstalledFlowAuthenticator");

    let client = hyper_util::client::legacy::Client::builder(hyper_util::rt::TokioExecutor::new())
        .build(
            hyper_rustls::HttpsConnectorBuilder::new()
                .with_native_roots()
                .unwrap()
                .https_or_http()
                .enable_http1()
                .build(),
        );

    DriveHub::new(client, auth)
}
