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

/// The settings the Microsoft sign-in commands need, or what is missing. They come from the environment:
/// COGNITO_ISSUER and COGNITO_CLIENT_ID (the same ones the API uses) and the Microsoft sign-in app's
/// MICROSOFT_SIGNIN_CLIENT_ID and MICROSOFT_SIGNIN_CLIENT_SECRET (a separate app from the connector's).
async fn microsoft_sign_in() -> workspace_cloud::Result<(workspace_cloud::signin::AwsSignIn, workspace_cloud::signin::MicrosoftApp)> {
    let need = |name: &str| std::env::var(name).ok().filter(|value| !value.trim().is_empty()).ok_or_else(|| workspace_cloud::Error::Config(format!("{name} is not set")));
    let (issuer, client) = (need("COGNITO_ISSUER")?, need("COGNITO_CLIENT_ID")?);
    let app = workspace_cloud::signin::MicrosoftApp { client_id: need("MICROSOFT_SIGNIN_CLIENT_ID")?, client_secret: need("MICROSOFT_SIGNIN_CLIENT_SECRET")? };
    Ok((workspace_cloud::signin::AwsSignIn::from_env(&issuer, &client).await?, app))
}

/// A database connection with the provisioner's login (DATABASE_URL).
async fn provisioner_connection() -> workspace_cloud::Result<deadpool_postgres::Object> {
    let settings = Settings::for_migration()?;
    build_pool(&settings, 1)?.get().await.map_err(|error| workspace_cloud::Error::Config(format!("database: {error}")))
}

fn parse_firm_id(text: Option<String>, usage: &str) -> workspace_cloud::Result<workspace_cloud::auth::TenantId> {
    text.and_then(|id| uuid::Uuid::parse_str(id.trim()).ok())
        .map(workspace_cloud::auth::TenantId)
        .ok_or_else(|| workspace_cloud::Error::Config(format!("usage: {usage}")))
}

/// `workspace-cloud provision-firm "Firm name" [--microsoft-tenant <Microsoft tenant ID>]` creates a firm
/// together with its own encryption key, and prints the firm's ID (the `custom:tenant_id` of its people in
/// Cognito) and the key's ARN. With `--microsoft-tenant` it also gives the firm Microsoft sign-in tied to
/// that Microsoft tenant,. Run it with the `app_provisioner` database login and your own AWS sign-in.
async fn provision_firm() -> workspace_cloud::Result<()> {
    let usage = || workspace_cloud::Error::Config("usage: workspace-cloud provision-firm \"Firm name\" [--microsoft-tenant <Microsoft tenant ID>]".into());
    let (mut name, mut microsoft) = (None, None);
    let mut args = std::env::args().skip(2);
    while let Some(argument) = args.next() {
        if argument == "--microsoft-tenant" {
            microsoft = Some(args.next().ok_or_else(usage)?);
        } else if name.is_none() {
            name = Some(argument);
        } else {
            return Err(usage());
        }
    }
    let name = name.filter(|name| !name.trim().is_empty()).ok_or_else(usage)?;
    // Everything Microsoft needs is checked before anything is made, so a typo leaves no firm half set up.
    let sign_in = match &microsoft {
        Some(tenant) => {
            let tenant = workspace_cloud::signin::check_entra_tenant(tenant)?;
            Some((tenant, microsoft_sign_in().await?))
        }
        None => None,
    };
    let conn = provisioner_connection().await?;
    let keys = workspace_cloud::kms::AwsKms::from_env().await;
    let (tenant, key) = workspace_cloud::kms::provision_firm(&conn, &keys, &name).await?;
    println!("tenant_id={}\nkms_key_arn={key}", tenant.0);
    if let Some((entra, (setup, app))) = sign_in {
        match workspace_cloud::signin::add_microsoft(&setup, tenant, &name, &entra, &app).await {
            Ok(provider) => println!("microsoft_provider={provider}"),
            Err(error) => {
                return Err(workspace_cloud::Error::SignIn(format!(
                    "the firm and its key were made, but Microsoft sign-in was not: {error}. Run add-microsoft {} {entra} once that is fixed",
                    tenant.0
                )));
            }
        }
    }
    Ok(())
}

/// `workspace-cloud add-microsoft <firm id> <Microsoft tenant ID>` gives an existing firm
/// Microsoft sign-in.
async fn add_microsoft_command() -> workspace_cloud::Result<()> {
    let usage = "workspace-cloud add-microsoft <firm id> <Microsoft tenant ID>";
    let mut args = std::env::args().skip(2);
    let firm = parse_firm_id(args.next(), usage)?;
    let entra = args.next().ok_or_else(|| workspace_cloud::Error::Config(format!("usage: {usage}")))?;
    let entra = workspace_cloud::signin::check_entra_tenant(&entra)?;
    let (setup, app) = microsoft_sign_in().await?;
    let conn = provisioner_connection().await?;
    let name: String = conn
        .query_opt("SELECT name FROM tenants WHERE id = $1", &[&firm.0])
        .await?
        .map(|row| row.get(0))
        .ok_or_else(|| workspace_cloud::Error::SignIn("there is no firm with that ID".into()))?;
    let provider = workspace_cloud::signin::add_microsoft(&setup, firm, &name, &entra, &app).await?;
    println!("microsoft_provider={provider}");
    Ok(())
}

/// `workspace-cloud remove-microsoft <firm id>` takes a firm's Microsoft sign-in away, for example when it leaves.
async fn remove_microsoft_command() -> workspace_cloud::Result<()> {
    let firm = parse_firm_id(std::env::args().nth(2), "workspace-cloud remove-microsoft <firm id>")?;
    let (setup, _) = microsoft_sign_in().await?;
    let removed = workspace_cloud::signin::remove_microsoft(&setup, firm).await?;
    if removed.is_empty() {
        println!("that firm has no Microsoft sign-in");
    }
    for provider in removed {
        println!("removed={provider}");
    }
    Ok(())
}

/// `workspace-cloud check-secrets <firm id>` tries the production token store end to end against real AWS:
/// it stores a test secret for the firm, reads it back, deletes it, and says whether all of that worked.
/// Run it with the API's database login and the AWS credentials the services use, after `provision-firm`.
/// It is the quickest way to learn that the key, the permissions and the database agree.
async fn check_secrets() -> workspace_cloud::Result<()> {
    use workspace_cloud::{auth::TenantId, secrets::refresh_token_name};
    let usage = || workspace_cloud::Error::Config("usage: workspace-cloud check-secrets <firm id>".into());
    let tenant = std::env::args().nth(2).and_then(|id| uuid::Uuid::parse_str(&id).ok()).map(TenantId).ok_or_else(usage)?;
    let settings = Settings::for_migration()?;
    if settings.allow_insecure {
        return Err(workspace_cloud::Error::Config("check-secrets tests the production store: unset ALLOW_INSECURE".into()));
    }
    let pool = build_pool(&settings, 2)?;
    let secrets = secrets_from_env(false, &pool).await?;
    let name = refresh_token_name(tenant, uuid::Uuid::new_v4());
    let value = format!("check-{}", uuid::Uuid::new_v4());
    let wrong = |reason: &str| workspace_cloud::Error::Crypto(reason.into());
    secrets.put(tenant, &name, &value).await?;
    let read = secrets.get(tenant, &name).await;
    // Clean up even if the read failed, so a half-working setup leaves nothing behind.
    secrets.delete(tenant, &name).await?;
    if read?.as_deref() != Some(value.as_str()) {
        return Err(wrong("the secret read back is not the one that was stored"));
    }
    if secrets.get(tenant, &name).await?.is_some() {
        return Err(wrong("the test secret is still there after it was deleted"));
    }
    println!("ok: stored, read back and deleted a test secret for firm {} under its own key", tenant.0);
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
        Some("check-secrets") => return check_secrets().await,
        Some("add-microsoft") => return add_microsoft_command().await,
        Some("remove-microsoft") => return remove_microsoft_command().await,
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
    let sign_in_route = sign_in_route(&settings).await;
    let app = api::router(AppState {
        verifier,
        jwks: Some(jwks),
        pool,
        oauth: Arc::new(oauth),
        secrets,
        embedder: load_embedder(),
    });
    let app = match sign_in_route {
        Some(route) => app.merge(route),
        None => app,
    };
    let listener = tokio::net::TcpListener::bind(&settings.bind)
        .await
        .map_err(|error| workspace_cloud::Error::Config(format!("{}: {error}", settings.bind)))?;
    tracing::info!(bind = %settings.bind, "listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await
        .map_err(|error| workspace_cloud::Error::Config(error.to_string()))
}

/// The unauthenticated "which Microsoft sign-in is mine" route, if this server can look people up in Cognito.
/// Without AWS access (local development) it is left out and the app shows Cognito's own page instead.
async fn sign_in_route(settings: &Settings) -> Option<axum::Router> {
    match workspace_cloud::signin::AwsSignIn::from_env(&settings.issuer, &settings.audience).await {
        Ok(lookup) => Some(workspace_cloud::signin_api::routes(Arc::new(lookup))),
        Err(error) => {
            tracing::warn!(%error, "the Microsoft sign-in lookup is switched off");
            None
        }
    }
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
