//! Microsoft sign-in for a firm, set up when the firm is, with no change to the infrastructure code.
//!
//! Cognito checks that a token's issuer matches the provider's exactly, and a Microsoft work account's
//! issuer belongs to the firm's own Microsoft tenant, so each firm that signs in with Microsoft needs its
//! own provider (see infra/signin.tf). Making one takes three steps, done here together and undone
//! together:
//!   1. record which firm the provider belongs to, in the parameter store (the pre sign-up function reads
//!      it there, and lets only people invited to that firm use the provider),
//!   2. create the provider in the user pool, tied to the firm's Microsoft tenant,
//!   3. allow the provider on the desktop app's client, which means rewriting that client's settings.
//!
//! Step 3 is the dangerous one: Cognito's update call resets every setting it is not given, so the code
//! copies every setting the client has now and changes only the list of providers.

use std::collections::HashMap;

use async_trait::async_trait;
use aws_sdk_cognitoidentityprovider as cognito;
use aws_sdk_ssm as ssm;

use crate::{Error, Result, auth::TenantId};

/// Where each provider's firm is recorded: `<path><provider name>` holds the firm's ID. The pre sign-up
/// function (infra/presignup) and its permission in infra/signin.tf use the same path.
/// The one provider for personal Microsoft accounts (`@outlook.com` and the like). They all sign in through one
/// fixed Microsoft address, so, unlike work accounts, one provider serves everybody.
pub const PERSONAL_PROVIDER: &str = "MicrosoftPersonal";
/// Microsoft's fixed tenant ID for personal accounts: the issuer of every one of their tokens.
const PERSONAL_ISSUER_TENANT: &str = "9188040d-6c67-4c5b-b112-36a304b66dad";

pub const PARAMETER_PATH: &str = "/prism/signin/microsoft/";

/// The Microsoft sign-in app: one multi-tenant registration shared by every firm. It asks only for the
/// person's name and email, and is not the connector's app.
#[derive(Clone)]
pub struct MicrosoftApp {
    pub client_id: String,
    pub client_secret: String,
}

fn failed(action: &str, error: impl std::fmt::Display) -> Error {
    Error::SignIn(format!("{action}: {error}"))
}

/// A provider name Cognito accepts and the pre sign-up function recognises: "ms-" and the firm's name,
/// then the first six characters of its ID so that two firms with one name never share one.
pub fn provider_name(firm_name: &str, firm: TenantId) -> String {
    let mut slug = String::new();
    for c in firm_name.to_lowercase().chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
        } else if !slug.ends_with('-') && !slug.is_empty() {
            slug.push('-');
        }
    }
    let slug: String = slug.trim_end_matches('-').chars().take(18).collect();
    let slug = slug.trim_end_matches('-');
    let short: String = firm.0.simple().to_string().chars().take(6).collect();
    if slug.is_empty() { format!("ms-{short}") } else { format!("ms-{slug}-{short}") }
}

pub fn parameter_name(provider: &str) -> String {
    format!("{PARAMETER_PATH}{provider}")
}

/// A Microsoft tenant ID, lowercased, or why it cannot be one.
pub fn check_entra_tenant(text: &str) -> Result<String> {
    let id = text.trim().to_lowercase();
    match uuid::Uuid::parse_str(&id) {
        Ok(parsed) if id.len() == 36 && !parsed.is_nil() => Ok(id),
        _ => Err(Error::SignIn("the Microsoft tenant ID must look like 123e4567-e89b-12d3-a456-426614174000".into())),
    }
}

/// The user pool's ID, taken from the token issuer address (`https://cognito-idp.<region>.amazonaws.com/<pool id>`).
pub fn pool_id_from_issuer(issuer: &str) -> Result<String> {
    let url = url::Url::parse(issuer).map_err(|_| Error::SignIn("COGNITO_ISSUER is not an address".into()))?;
    let pool = url.path().trim_matches('/');
    if url.host_str().is_some_and(|host| host.starts_with("cognito-idp.")) && !pool.is_empty() && !pool.contains('/') {
        Ok(pool.to_string())
    } else {
        Err(Error::SignIn("COGNITO_ISSUER must look like https://cognito-idp.<region>.amazonaws.com/<pool id>".into()))
    }
}

#[async_trait]
pub trait SignInSetup: Send + Sync {
    /// Records the firm a provider belongs to. Refuses to replace an existing record.
    async fn put_firm(&self, provider: &str, firm: TenantId) -> Result<()>;
    async fn delete_firm(&self, provider: &str) -> Result<()>;
    /// The providers recorded for a firm.
    async fn providers_of(&self, firm: TenantId) -> Result<Vec<String>>;
    async fn create_microsoft_provider(&self, provider: &str, entra_tenant: &str, app: &MicrosoftApp) -> Result<()>;
    /// The shared provider for personal Microsoft accounts.
    async fn create_personal_microsoft_provider(&self, app: &MicrosoftApp) -> Result<()>;
    async fn delete_provider(&self, provider: &str) -> Result<()>;
    async fn allow_on_app(&self, provider: &str) -> Result<()>;
    async fn disallow_on_app(&self, provider: &str) -> Result<()>;
}

/// Gives a firm Microsoft sign-in and returns the provider's name. If any step fails, the steps already
/// taken are undone, so a half-made provider is never left behind.
pub async fn add_microsoft(setup: &dyn SignInSetup, firm: TenantId, firm_name: &str, entra_tenant: &str, app: &MicrosoftApp) -> Result<String> {
    let entra_tenant = check_entra_tenant(entra_tenant)?;
    if !setup.providers_of(firm).await?.is_empty() {
        return Err(Error::SignIn("this firm already has Microsoft sign-in: remove it first (remove-microsoft)".into()));
    }
    let provider = provider_name(firm_name, firm);
    setup.put_firm(&provider, firm).await?;
    if let Err(error) = setup.create_microsoft_provider(&provider, &entra_tenant, app).await {
        let _ = setup.delete_firm(&provider).await;
        return Err(error);
    }
    if let Err(error) = setup.allow_on_app(&provider).await {
        let _ = setup.delete_provider(&provider).await;
        let _ = setup.delete_firm(&provider).await;
        return Err(error);
    }
    Ok(provider)
}

/// Turns on sign-in with personal Microsoft accounts for everyone. If it cannot be allowed on the app, the provider
/// is removed again, so nothing is left half made.
pub async fn add_personal_microsoft(setup: &dyn SignInSetup, app: &MicrosoftApp) -> Result<()> {
    setup.create_personal_microsoft_provider(app).await?;
    if let Err(error) = setup.allow_on_app(PERSONAL_PROVIDER).await {
        let _ = setup.delete_provider(PERSONAL_PROVIDER).await;
        return Err(error);
    }
    Ok(())
}

/// Turns personal Microsoft sign-in off. It stops being offered first, then the provider goes.
pub async fn remove_personal_microsoft(setup: &dyn SignInSetup) -> Result<()> {
    setup.disallow_on_app(PERSONAL_PROVIDER).await?;
    setup.delete_provider(PERSONAL_PROVIDER).await
}

/// Takes a firm's Microsoft sign-in away and returns the providers removed. The record goes first, so
/// sign-ins stop at once even if a later step fails.
pub async fn remove_microsoft(setup: &dyn SignInSetup, firm: TenantId) -> Result<Vec<String>> {
    let providers = setup.providers_of(firm).await?;
    for provider in &providers {
        setup.delete_firm(provider).await?;
        setup.disallow_on_app(provider).await?;
        setup.delete_provider(provider).await?;
    }
    Ok(providers)
}

/// Which Microsoft provider the person with this email should be sent to.
#[async_trait]
pub trait SignInRouting: Send + Sync {
    /// The provider of the firm the email was invited to, or `None` if nobody was invited with that email or
    /// the firm has no Microsoft sign-in. The two cases are deliberately not told apart.
    async fn provider_for_email(&self, email: &str) -> Result<Option<String>>;
}

/// Cognito and the parameter store, on whatever credentials the process has.
pub struct AwsSignIn {
    cognito: cognito::Client,
    ssm: ssm::Client,
    pool_id: String,
    app_client_id: String,
}

impl AwsSignIn {
    pub async fn from_env(issuer: &str, app_client_id: &str) -> Result<Self> {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest()).load().await;
        Ok(Self::new(cognito::Client::new(&config), ssm::Client::new(&config), pool_id_from_issuer(issuer)?, app_client_id.to_string()))
    }

    pub fn new(cognito: cognito::Client, ssm: ssm::Client, pool_id: String, app_client_id: String) -> Self {
        Self { cognito, ssm, pool_id, app_client_id }
    }

    async fn create_microsoft_oidc(&self, provider: &str, login: &str, issuer: &str, app: &MicrosoftApp) -> Result<()> {
        let details: HashMap<String, String> = [
            ("client_id", app.client_id.clone()),
            ("client_secret", app.client_secret.clone()),
            ("authorize_scopes", "openid email profile".into()),
            ("attributes_request_method", "GET".into()),
            ("oidc_issuer", issuer.to_string()),
            ("authorize_url", format!("{login}/oauth2/v2.0/authorize")),
            ("token_url", format!("{login}/oauth2/v2.0/token")),
            ("attributes_url", "https://graph.microsoft.com/oidc/userinfo".into()),
            ("jwks_uri", format!("{login}/discovery/v2.0/keys")),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
        self.cognito
            .create_identity_provider()
            .user_pool_id(&self.pool_id)
            .provider_name(provider)
            .provider_type(cognito::types::IdentityProviderTypeType::Oidc)
            .set_provider_details(Some(details))
            .set_attribute_mapping(Some(HashMap::from([("email".to_string(), "email".to_string()), ("username".to_string(), "sub".to_string())])))
            .send()
            .await
            .map_err(|error| failed("could not create the Microsoft provider", cognito::error::DisplayErrorContext(&error)))?;
        Ok(())
    }

    /// The desktop app's client as it is now, and its settings handed back for an update, with the
    /// providers replaced by `providers`. Every setting is carried over.
    async fn rewrite_app(&self, change: impl FnOnce(&mut Vec<String>)) -> Result<()> {
        let described = self
            .cognito
            .describe_user_pool_client()
            .user_pool_id(&self.pool_id)
            .client_id(&self.app_client_id)
            .send()
            .await
            .map_err(|error| failed("could not read the app's settings", cognito::error::DisplayErrorContext(&error)))?;
        let current = described.user_pool_client.ok_or_else(|| Error::SignIn("AWS returned no app settings".into()))?;
        let mut providers = current.supported_identity_providers.clone().unwrap_or_default();
        let before = providers.clone();
        change(&mut providers);
        if providers == before {
            return Ok(());
        }
        self.cognito
            .update_user_pool_client()
            .user_pool_id(&self.pool_id)
            .client_id(&self.app_client_id)
            .set_client_name(current.client_name.clone())
            .set_refresh_token_validity(Some(current.refresh_token_validity))
            .set_access_token_validity(current.access_token_validity)
            .set_id_token_validity(current.id_token_validity)
            .set_token_validity_units(current.token_validity_units.clone())
            .set_read_attributes(current.read_attributes.clone())
            .set_write_attributes(current.write_attributes.clone())
            .set_explicit_auth_flows(current.explicit_auth_flows.clone())
            .set_supported_identity_providers(Some(providers))
            .set_callback_urls(current.callback_urls.clone())
            .set_logout_urls(current.logout_urls.clone())
            .set_default_redirect_uri(current.default_redirect_uri.clone())
            .set_allowed_o_auth_flows(current.allowed_o_auth_flows.clone())
            .set_allowed_o_auth_scopes(current.allowed_o_auth_scopes.clone())
            .set_allowed_o_auth_flows_user_pool_client(current.allowed_o_auth_flows_user_pool_client)
            .set_analytics_configuration(current.analytics_configuration.clone())
            .set_prevent_user_existence_errors(current.prevent_user_existence_errors.clone())
            .set_enable_token_revocation(current.enable_token_revocation)
            .set_enable_propagate_additional_user_context_data(current.enable_propagate_additional_user_context_data)
            .set_auth_session_validity(current.auth_session_validity)
            .set_refresh_token_rotation(current.refresh_token_rotation.clone())
            .send()
            .await
            .map_err(|error| failed("could not update the app's settings", cognito::error::DisplayErrorContext(&error)))?;
        Ok(())
    }
}

#[async_trait]
impl SignInSetup for AwsSignIn {
    async fn put_firm(&self, provider: &str, firm: TenantId) -> Result<()> {
        self.ssm
            .put_parameter()
            .name(parameter_name(provider))
            .value(firm.0.to_string())
            .r#type(ssm::types::ParameterType::String)
            .description("The firm a Microsoft sign-in provider belongs to (written by workspace-cloud)")
            .overwrite(false)
            .send()
            .await
            .map_err(|error| failed("could not record the firm for the provider", ssm::error::DisplayErrorContext(&error)))?;
        Ok(())
    }

    async fn delete_firm(&self, provider: &str) -> Result<()> {
        match self.ssm.delete_parameter().name(parameter_name(provider)).send().await {
            Ok(_) => Ok(()),
            Err(error) if error.as_service_error().is_some_and(|e| e.is_parameter_not_found()) => Ok(()),
            Err(error) => Err(failed("could not remove the provider's record", ssm::error::DisplayErrorContext(&error))),
        }
    }

    async fn providers_of(&self, firm: TenantId) -> Result<Vec<String>> {
        let wanted = firm.0.to_string();
        let mut found = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .ssm
                .get_parameters_by_path()
                .path(PARAMETER_PATH.trim_end_matches('/'))
                .max_results(10)
                .set_next_token(token.clone())
                .send()
                .await
                .map_err(|error| failed("could not list the providers", ssm::error::DisplayErrorContext(&error)))?;
            for parameter in page.parameters() {
                if parameter.value() == Some(wanted.as_str())
                    && let Some(name) = parameter.name().and_then(|name| name.strip_prefix(PARAMETER_PATH))
                {
                    found.push(name.to_string());
                }
            }
            token = page.next_token().map(str::to_string);
            if token.is_none() {
                return Ok(found);
            }
        }
    }

    async fn create_microsoft_provider(&self, provider: &str, entra_tenant: &str, app: &MicrosoftApp) -> Result<()> {
        let login = format!("https://login.microsoftonline.com/{entra_tenant}");
        self.create_microsoft_oidc(provider, &login, &format!("{login}/v2.0"), app).await
    }

    async fn create_personal_microsoft_provider(&self, app: &MicrosoftApp) -> Result<()> {
        // Sign-in goes through the "consumers" address, but the token names Microsoft's fixed personal tenant.
        let issuer = format!("https://login.microsoftonline.com/{PERSONAL_ISSUER_TENANT}/v2.0");
        self.create_microsoft_oidc(PERSONAL_PROVIDER, "https://login.microsoftonline.com/consumers", &issuer, app).await
    }

    async fn delete_provider(&self, provider: &str) -> Result<()> {
        match self.cognito.delete_identity_provider().user_pool_id(&self.pool_id).provider_name(provider).send().await {
            Ok(_) => Ok(()),
            Err(error) if error.as_service_error().is_some_and(|e| e.is_resource_not_found_exception()) => Ok(()),
            Err(error) => Err(failed("could not delete the provider", cognito::error::DisplayErrorContext(&error))),
        }
    }

    async fn allow_on_app(&self, provider: &str) -> Result<()> {
        let provider = provider.to_string();
        self.rewrite_app(|providers| {
            if !providers.contains(&provider) {
                providers.push(provider);
            }
        })
        .await
    }

    async fn disallow_on_app(&self, provider: &str) -> Result<()> {
        let provider = provider.to_string();
        self.rewrite_app(|providers| providers.retain(|existing| existing != &provider)).await
    }
}

#[async_trait]
impl SignInRouting for AwsSignIn {
    async fn provider_for_email(&self, email: &str) -> Result<Option<String>> {
        let user = match self.cognito.admin_get_user().user_pool_id(&self.pool_id).username(email).send().await {
            Ok(user) => user,
            Err(error) if error.as_service_error().is_some_and(|e| e.is_user_not_found_exception()) => return Ok(None),
            Err(error) => return Err(failed("could not look up the invitation", cognito::error::DisplayErrorContext(&error))),
        };
        let firm = user
            .user_attributes()
            .iter()
            .find(|attribute| attribute.name() == "custom:tenant_id")
            .and_then(|attribute| attribute.value())
            .and_then(|value| uuid::Uuid::parse_str(value.trim()).ok());
        match firm {
            Some(id) => Ok(self.providers_of(TenantId(id)).await?.into_iter().next()),
            None => Ok(None),
        }
    }
}

#[async_trait]
impl crate::onboarding::Accounts for AwsSignIn {
    async fn has_firm(&self, username: &str) -> Result<bool> {
        let user = self
            .cognito
            .admin_get_user()
            .user_pool_id(&self.pool_id)
            .username(username)
            .send()
            .await
            .map_err(|error| failed("could not read the account", cognito::error::DisplayErrorContext(&error)))?;
        Ok(user.user_attributes().iter().any(|attribute| matches!(attribute.name(), "custom:tenant_id" | "custom:firm_id") && attribute.value().is_some_and(|value| !value.trim().is_empty())))
    }

    async fn make_admin_of(&self, username: &str, firm: TenantId) -> Result<()> {
        // `custom:tenant_id` can only be given when an account is made, so a person who starts their own firm gets
        // `custom:firm_id`, which can be changed. The server treats either as the person's firm.
        let attribute = cognito::types::AttributeType::builder()
            .name("custom:firm_id")
            .value(firm.0.to_string())
            .build()
            .map_err(|error| Error::SignIn(error.to_string()))?;
        self.cognito
            .admin_update_user_attributes()
            .user_pool_id(&self.pool_id)
            .username(username)
            .user_attributes(attribute)
            .send()
            .await
            .map_err(|error| failed("could not attach the account to its firm", cognito::error::DisplayErrorContext(&error)))?;
        if let Err(error) = self
            .cognito
            .admin_add_user_to_group()
            .user_pool_id(&self.pool_id)
            .username(username)
            .group_name(crate::auth::ADMIN_GROUP)
            .send()
            .await
        {
            return Err(failed("could not make the account its firm's administrator", cognito::error::DisplayErrorContext(&error)));
        }
        Ok(())
    }
}

#[async_trait]
impl crate::prune::PoolUsers for AwsSignIn {
    async fn firmless_federated(&self) -> Result<Vec<crate::prune::Newcomer>> {
        let mut found = Vec::new();
        let mut token: Option<String> = None;
        loop {
            let page = self
                .cognito
                .list_users()
                .user_pool_id(&self.pool_id)
                .limit(60)
                .set_pagination_token(token.clone())
                .send()
                .await
                .map_err(|error| failed("could not list the accounts", cognito::error::DisplayErrorContext(&error)))?;
            for user in page.users() {
                let attribute = |name: &str| user.attributes().iter().find(|a| a.name() == name).and_then(|a| a.value()).map(str::to_string);
                let firmless = ["custom:tenant_id", "custom:firm_id"].iter().all(|name| attribute(name).is_none_or(|value| value.trim().is_empty()));
                let outside = user.user_status() == Some(&cognito::types::UserStatusType::ExternalProvider);
                if let (true, true, Some(username), Some(created)) = (firmless, outside, user.username(), user.user_create_date()) {
                    found.push(crate::prune::Newcomer { username: username.to_string(), email: attribute("email").unwrap_or_default(), created: created.secs() });
                }
            }
            token = page.pagination_token().map(str::to_string);
            if token.is_none() {
                return Ok(found);
            }
        }
    }

    async fn has_firm(&self, username: &str) -> Result<bool> {
        crate::onboarding::Accounts::has_firm(self, username).await
    }

    async fn delete(&self, username: &str) -> Result<()> {
        match self.cognito.admin_delete_user().user_pool_id(&self.pool_id).username(username).send().await {
            Ok(_) => Ok(()),
            Err(error) if error.as_service_error().is_some_and(|e| e.is_user_not_found_exception()) => Ok(()),
            Err(error) => Err(failed("could not delete the account", cognito::error::DisplayErrorContext(&error))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_provider_name_is_short_safe_and_never_shared_between_firms() {
        let (a, b) = (TenantId(uuid::Uuid::parse_str("aaaaaa11-1111-4111-8111-111111111111").unwrap()), TenantId(uuid::Uuid::parse_str("bbbbbb22-2222-4222-8222-222222222222").unwrap()));
        assert_eq!(provider_name("Acme Law, LLP", a), "ms-acme-law-llp-aaaaaa");
        assert_ne!(provider_name("Acme", a), provider_name("Acme", b), "one name, two firms");
        assert_eq!(provider_name("  ", a), "ms-aaaaaa");
        assert_eq!(provider_name("!!!", b), "ms-bbbbbb");
        let long = provider_name("A very long firm name indeed, longer than thirty-two", a);
        assert!(long.len() <= 32 && long.starts_with("ms-") && !long.contains('_') && !long.contains("--"), "{long}");
        // Names that look like paths cannot climb out of the parameter path.
        assert!(!provider_name("../../etc/passwd", a).contains('/'));
        assert_eq!(parameter_name("ms-acme-aaaaaa"), "/prism/signin/microsoft/ms-acme-aaaaaa");
    }

    #[test]
    fn a_microsoft_tenant_id_must_be_a_real_id() {
        assert_eq!(check_entra_tenant(" 123E4567-E89B-12D3-A456-426614174000 ").unwrap(), "123e4567-e89b-12d3-a456-426614174000");
        for bad in ["", "common", "organizations", "123e4567e89b12d3a456426614174000", "00000000-0000-0000-0000-000000000000", "123e4567-e89b-12d3-a456-42661417400/../x"] {
            assert!(check_entra_tenant(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_pool_id_is_read_from_the_issuer_and_nothing_else_is_accepted() {
        assert_eq!(pool_id_from_issuer("https://cognito-idp.us-east-2.amazonaws.com/us-east-2_AbC123").unwrap(), "us-east-2_AbC123");
        for bad in ["", "not an address", "https://example.com/us-east-2_AbC123", "https://cognito-idp.us-east-2.amazonaws.com/", "https://cognito-idp.us-east-2.amazonaws.com/a/b"] {
            assert!(pool_id_from_issuer(bad).is_err(), "{bad:?}");
        }
    }
}
