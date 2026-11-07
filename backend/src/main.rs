use std::sync::Arc;

use workspace_cloud::{
    api::{self, AppState},
    auth::TokenVerifier,
    jwks::JwksSource,
    server::{Settings, build_pool, oauth_from_env, secrets_from_env},
};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    if let Err(error) = run().await {
        tracing::error!(%error, "could not start");
        std::process::exit(1);
    }
}

/// `workspace-cloud migrate` applies pending database migrations and exits. Run it with the
/// deployment database user, never with an application role.
async fn migrate() -> workspace_cloud::Result<()> {
    let settings = Settings::for_migration()?;
    let pool = build_pool(&settings, 1)?;
    let mut conn = pool
        .get()
        .await
        .map_err(|error| workspace_cloud::Error::Config(format!("database: {error}")))?;
    workspace_cloud::db::migrate(&mut conn).await?;
    tracing::info!("migrations are up to date");
    Ok(())
}

/// `workspace-cloud provision-firm "Firm name"` creates a firm together with its own encryption key,
/// and prints the firm's ID (the `custom:tenant_id` of its people in Cognito) and the key's ARN. Run
/// it with the `app_provisioner` database login and AWS credentials that may create KMS keys.
async fn provision_firm() -> workspace_cloud::Result<()> {
    let name = std::env::args().nth(2).unwrap_or_default();
    let settings = Settings::for_migration()?;
    let pool = build_pool(&settings, 1)?;
    let conn = pool
        .get()
        .await
        .map_err(|error| workspace_cloud::Error::Config(format!("database: {error}")))?;
    let keys = workspace_cloud::kms::AwsKms::from_env().await;
    let (tenant, key) = workspace_cloud::kms::provision_firm(&conn, &keys, &name).await?;
    println!("tenant_id={}\nkms_key_arn={key}", tenant.0);
    Ok(())
}

/// `workspace-cloud worker` keeps connected sources up to date. Run it with the `app_worker`
/// database login.
async fn worker() -> workspace_cloud::Result<()> {
    let settings = Settings::for_migration()?;
    if settings.allow_insecure {
        tracing::warn!("ALLOW_INSECURE is on: for local development only");
    }
    let oauth = oauth_from_env(settings.allow_insecure)?;
    if !oauth.is_enabled() {
        return Err(workspace_cloud::Error::Config(
            "set OAUTH_STATE_KEY, OAUTH_REDIRECT_URI and a provider's client ID and secret".into(),
        ));
    }
    let pool = build_pool(&settings, 8)?;
    let secrets = secrets_from_env(settings.allow_insecure, &pool).await?;
    tracing::info!("worker started");
    // The file reader runs as its own restricted program. Without it, PDF and Office files
    // could only be found by name, so the worker refuses to start rather than quietly do less.
    let reader = workspace_cloud::extract_process::SubprocessExtractor::locate()
        .map_err(|error| workspace_cloud::Error::Config(error.to_string()))?;
    // Scanned pages and pictures are read with OCR when Tesseract is installed. Without it they
    // are found by name only, as before.
    let ocr = workspace_cloud::ocr::TesseractOcr::detect();
    match &ocr {
        Some(engine) => tracing::info!(pdfs = engine.reads_pdfs(), "OCR is on"),
        None => tracing::warn!(
            "OCR is off: Tesseract was not found, so scans and pictures are not read"
        ),
    }
    let extractor = workspace_cloud::ocr::OcrExtractor::new(
        reader,
        ocr.map(|engine| {
            std::sync::Arc::new(engine) as std::sync::Arc<dyn workspace_cloud::ocr::Ocr>
        }),
    );
    workspace_cloud::worker::run_forever(
        pool,
        secrets.as_ref(),
        &oauth,
        &extractor,
        load_embedder(),
        load_analyzer(),
        load_agent_model(),
    )
    .await;
    Ok(())
}

/// The meaning-search model, or none if it cannot be loaded (searches then match words only).
fn load_embedder() -> Option<Arc<dyn workspace_cloud::embed::Embedder>> {
    if std::env::var("EMBEDDINGS_DISABLED").is_ok_and(|value| value == "1") {
        tracing::warn!("meaning search is off: EMBEDDINGS_DISABLED is set");
        return None;
    }
    match workspace_cloud::embed::LocalEmbedder::load() {
        Ok(embedder) => {
            tracing::info!(
                model = workspace_cloud::embed::MODEL_NAME,
                "meaning search is on"
            );
            Some(Arc::new(embedder))
        }
        Err(error) => {
            tracing::warn!(%error, "meaning search is off");
            None
        }
    }
}

/// The model that reads documents for company knowledge, or none without `ANTHROPIC_API_KEY`.
fn load_analyzer() -> Option<Arc<dyn workspace_cloud::knowledge::Analyzer>> {
    match workspace_cloud::knowledge::ClaudeAnalyzer::from_env() {
        Some(analyzer) => {
            tracing::info!("company knowledge can be switched on: a Claude key is set");
            Some(Arc::new(analyzer))
        }
        None => {
            tracing::warn!("company knowledge is unavailable here: it needs ANTHROPIC_API_KEY, and the direct Claude API is only allowed for local development (ALLOW_INSECURE=1)");
            None
        }
    }
}

/// The model that shared agents work with, or none without `ANTHROPIC_API_KEY`.
fn load_agent_model() -> Option<Arc<dyn workspace_cloud::agent_runner::AgentModel>> {
    match workspace_cloud::agent_runner::ClaudeAgentModel::from_env() {
        Some(model) => Some(Arc::new(model)),
        None => {
            tracing::warn!("shared agents cannot run here: they need ANTHROPIC_API_KEY, and the direct Claude API is only allowed for local development (ALLOW_INSECURE=1)");
            None
        }
    }
}

async fn run() -> workspace_cloud::Result<()> {
    match std::env::args().nth(1).as_deref() {
        Some("migrate") => return migrate().await,
        Some("worker") => return worker().await,
        Some("provision-firm") => return provision_firm().await,
        Some("download-model") => {
            // Fetches the meaning-search model once, so the server and the worker do not both
            // try to download it at the same moment.
            return workspace_cloud::embed::LocalEmbedder::load()
                .map(|_| println!("model ready"))
                .map_err(|error| workspace_cloud::Error::Embedding(error.to_string()));
        }
        _ => {}
    }
    let settings = Settings::from_env()?;
    if settings.allow_insecure {
        tracing::warn!("ALLOW_INSECURE is on: for local development only");
    }
    let verifier = Arc::new(TokenVerifier::new(&settings.issuer, &settings.audience));
    let jwks = Arc::new(JwksSource::new(settings.jwks_url()?)?);
    // Starting without the signing keys would mean refusing everyone, so stop instead.
    jwks.load(&verifier).await?;
    let pool = build_pool(&settings, 16)?;

    let oauth = oauth_from_env(settings.allow_insecure)?;
    if !oauth.is_enabled() {
        tracing::warn!(
            "connectors are switched off: OAUTH_STATE_KEY and provider credentials are not set"
        );
    }
    let secrets = secrets_from_env(settings.allow_insecure, &pool).await?;
    let app = api::router(AppState {
        verifier,
        jwks: Some(jwks),
        pool,
        oauth: Arc::new(oauth),
        secrets,
        embedder: load_embedder(),
    });
    let listener = tokio::net::TcpListener::bind(&settings.bind)
        .await
        .map_err(|error| workspace_cloud::Error::Config(format!("{}: {error}", settings.bind)))?;
    tracing::info!(bind = %settings.bind, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|error| workspace_cloud::Error::Config(error.to_string()))
}

async fn shutdown() {
    let ctrl_c = tokio::signal::ctrl_c();
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("SIGTERM handler");
        tokio::select! { _ = ctrl_c => {}, _ = terminate.recv() => {} }
    }
    #[cfg(not(unix))]
    ctrl_c.await.ok();
}
