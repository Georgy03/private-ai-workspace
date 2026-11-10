//! Turns a bearer token into the firm and user a request is for. The firm ID comes only from a
//! verified token, never from a request body, header or URL.

use std::{collections::HashMap, sync::RwLock};

use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, jwk::JwkSet};
use serde::Deserialize;
use uuid::Uuid;

use crate::{Error, Result};

/// Cognito custom attribute that carries the firm ID. Set by the server at sign-up; users cannot
/// edit it because the attribute is not writable by the app client.
pub const TENANT_CLAIM: &str = "custom:tenant_id";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TenantId(pub Uuid);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    pub tenant: TenantId,
    pub user: String,
    pub groups: Vec<String>,
    /// Lower-case, and present only when the identity provider verified it.
    pub email: Option<String>,
}

/// Members of this group may connect and disconnect sources for their firm.
pub const ADMIN_GROUP: &str = "firm_admin";

impl Principal {
    /// The identities a file's `readable_by` list is matched against: the sign-in ID, the groups
    /// the identity provider reports, and the verified email address and its domain, which is how
    /// Microsoft and Google name people in file permissions.
    pub fn readers(&self) -> Vec<String> {
        let mut readers = vec![format!("user:{}", self.user)];
        readers.extend(self.groups.iter().map(|group| format!("group:{group}")));
        if let Some(email) = &self.email {
            readers.push(format!("email:{email}"));
            if let Some((_, domain)) = email.rsplit_once('@') {
                readers.push(format!("domain:{domain}"));
            }
        }
        readers
    }

    pub fn is_admin(&self) -> bool {
        self.groups.iter().any(|group| group == ADMIN_GROUP)
    }
}

#[derive(Deserialize)]
struct Claims {
    sub: String,
    #[serde(rename = "custom:tenant_id")]
    tenant_id: Option<String>,
    /// The firm of someone who started their own firm from the app (the invited have `tenant_id`, which can never
    /// be changed and so always wins).
    #[serde(rename = "custom:firm_id")]
    firm_id: Option<String>,
    #[serde(rename = "cognito:groups", default)]
    groups: Vec<String>,
    token_use: Option<String>,
    #[serde(rename = "cognito:username")]
    username: Option<String>,
    email: Option<String>,
    email_verified: Option<serde_json::Value>,
}

impl Claims {
    /// The person's firm: the locked `tenant_id` of an invited person if it holds one, otherwise the `firm_id` of
    /// someone who started their own firm.
    fn firm(&self) -> Option<Uuid> {
        let parse = |value: &Option<String>| value.as_deref().and_then(|v| Uuid::parse_str(v).ok());
        parse(&self.tenant_id).or_else(|| parse(&self.firm_id))
    }
}

/// Lower-case, and only when the identity provider verified it. Cognito reports email_verified as a boolean,
/// federated providers sometimes as a string.
fn verified_email(claims: &Claims) -> Option<String> {
    let verified = match &claims.email_verified {
        Some(serde_json::Value::Bool(value)) => *value,
        Some(serde_json::Value::String(value)) => value == "true",
        _ => false,
    };
    claims
        .email
        .clone()
        .filter(|_| verified)
        .map(|email| email.trim().to_lowercase())
        .filter(|email| email.contains('@'))
}

/// Someone signed in who belongs to no firm yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewPerson {
    /// Their name in the user pool, which is what administrators' calls address.
    pub username: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Unassigned {
    New(NewPerson),
    AlreadyInFirm,
}

/// Verifies sign-in tokens against the identity provider's published signing keys.
pub struct TokenVerifier {
    keys: RwLock<HashMap<String, DecodingKey>>,
    validation: Validation,
}

impl TokenVerifier {
    /// Only RS256 is accepted, so a token cannot choose a weaker algorithm or "none".
    pub fn new(issuer: &str, audience: &str) -> Self {
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[issuer]);
        validation.set_audience(&[audience]);
        validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
        validation.leeway = 30;
        Self {
            keys: RwLock::new(HashMap::new()),
            validation,
        }
    }

    /// A verifier with one fixed key and no key ID. Used by tests and local development.
    pub fn with_pem(public_key_pem: &[u8], issuer: &str, audience: &str) -> Result<Self> {
        let verifier = Self::new(issuer, audience);
        let key = DecodingKey::from_rsa_pem(public_key_pem)
            .map_err(|error| Error::Token(format!("bad public key: {error}")))?;
        verifier.keys.write().unwrap().insert(String::new(), key);
        Ok(verifier)
    }

    /// Replaces the signing keys with a JWKS document (the format Cognito publishes).
    pub fn set_jwks(&self, jwks: &str) -> Result<()> {
        let set: JwkSet = serde_json::from_str(jwks)
            .map_err(|error| Error::Token(format!("bad key set: {error}")))?;
        let mut keys = HashMap::new();
        for jwk in &set.keys {
            let Some(kid) = jwk.common.key_id.clone() else {
                continue;
            };
            if let Ok(key) = DecodingKey::from_jwk(jwk) {
                keys.insert(kid, key);
            }
        }
        if keys.is_empty() {
            return Err(Error::Token("key set holds no usable keys".into()));
        }
        *self.keys.write().unwrap() = keys;
        Ok(())
    }

    pub fn has_key(&self, kid: &str) -> bool {
        self.keys.read().unwrap().contains_key(kid)
    }

    /// The key ID a token names, so a caller can refresh the key set when it is new.
    pub fn key_id(token: &str) -> Option<String> {
        jsonwebtoken::decode_header(token).ok()?.kid
    }

    /// The signed, unexpired ID token's claims, or why it is refused.
    fn decode_id(&self, token: &str) -> Result<Claims> {
        let header =
            jsonwebtoken::decode_header(token).map_err(|error| Error::Token(error.to_string()))?;
        let claims = {
            let keys = self.keys.read().unwrap();
            let key = keys
                .get(&header.kid.unwrap_or_default())
                .ok_or_else(|| Error::Token("unknown signing key".into()))?;
            decode::<Claims>(token, key, &self.validation)
                .map_err(|error| Error::Token(error.to_string()))?
                .claims
        };
        // Access tokens carry no firm attribute and have a different audience, but refuse them
        // by name too rather than rely on that.
        if claims.token_use.as_deref() != Some("id") {
            return Err(Error::Token("not an ID token".into()));
        }
        Ok(claims)
    }

    /// For the one request a person with no firm yet may make: starting their own. The token must be a
    /// valid ID token whose email the identity provider verified. A token that already carries a firm is
    /// reported as such, so the caller can say so instead of making a second firm.
    pub fn verify_unassigned(&self, token: &str) -> Result<Unassigned> {
        let claims = self.decode_id(token)?;
        if claims.firm().is_some() {
            return Ok(Unassigned::AlreadyInFirm);
        }
        let username = claims
            .username
            .clone()
            .filter(|name| !name.is_empty())
            .ok_or_else(|| Error::Token("token carries no user name".into()))?;
        let email = verified_email(&claims)
            .ok_or_else(|| Error::Token("the email address is not verified".into()))?;
        Ok(Unassigned::New(NewPerson { username, email }))
    }

    pub fn verify(&self, token: &str) -> Result<Principal> {
        let claims = self.decode_id(token)?;
        let tenant = claims.firm().ok_or_else(|| Error::Token("token carries no valid firm".into()))?;
        if claims.sub.is_empty() {
            return Err(Error::Token("token carries no user".into()));
        }
        let email = verified_email(&claims);
        Ok(Principal {
            tenant: TenantId(tenant),
            user: claims.sub,
            groups: claims.groups,
            email,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jsonwebtoken::{EncodingKey, Header, encode};
    use serde_json::{Value, json};

    const PRIVATE: &[u8] = include_bytes!("../tests/fixtures/test_rsa_private.pem");
    const PUBLIC: &[u8] = include_bytes!("../tests/fixtures/test_rsa_public.pem");
    const OTHER_PRIVATE: &[u8] = include_bytes!("../tests/fixtures/other_rsa_private.pem");
    const ISSUER: &str = "https://issuer.example/pool";
    const AUDIENCE: &str = "desktop-client";

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    }

    fn claims(tenant: Option<&str>) -> Value {
        let mut value = json!({
            "sub": "user-1", "iss": ISSUER, "aud": AUDIENCE, "exp": now() + 600, "token_use": "id",
            "cognito:groups": ["partners"],
        });
        if let Some(tenant) = tenant {
            value[TENANT_CLAIM] = json!(tenant);
        }
        value
    }

    fn sign(claims: &Value, key: &[u8]) -> String {
        encode(
            &Header::new(Algorithm::RS256),
            claims,
            &EncodingKey::from_rsa_pem(key).unwrap(),
        )
        .unwrap()
    }

    fn verifier() -> TokenVerifier {
        TokenVerifier::with_pem(PUBLIC, ISSUER, AUDIENCE).unwrap()
    }

    const TENANT: &str = "11111111-1111-4111-8111-111111111111";

    #[test]
    fn someone_who_started_their_own_firm_is_in_it_and_an_invited_persons_locked_firm_always_wins() {
        const OWN: &str = "22222222-2222-4222-8222-222222222222";
        let with = |tenant: Option<&str>, firm: Option<&str>| {
            let mut c = claims(tenant);
            if let Some(firm) = firm {
                c["custom:firm_id"] = json!(firm);
            }
            verifier().verify(&sign(&c, PRIVATE))
        };
        assert_eq!(with(None, Some(OWN)).unwrap().tenant, TenantId(Uuid::parse_str(OWN).unwrap()));
        // Both present: the locked one wins, so an invited person can never be moved by the changeable one.
        assert_eq!(with(Some(TENANT), Some(OWN)).unwrap().tenant, TenantId(Uuid::parse_str(TENANT).unwrap()));
        // Nothing usable in either: no firm.
        assert!(with(None, Some("not-a-uuid")).is_err());
        assert!(with(None, None).is_err());
        // A token with only the changeable one counts as already in a firm when asked to start another.
        let own = sign(&{ let mut c = claims(None); c["custom:firm_id"] = json!(OWN); c }, PRIVATE);
        assert!(matches!(verifier().verify_unassigned(&own).unwrap(), Unassigned::AlreadyInFirm));
    }

    #[test]
    fn a_valid_token_yields_the_firm_user_and_groups() {
        let principal = verifier()
            .verify(&sign(&claims(Some(TENANT)), PRIVATE))
            .unwrap();
        assert_eq!(principal.tenant, TenantId(Uuid::parse_str(TENANT).unwrap()));
        assert_eq!(principal.readers(), vec!["user:user-1", "group:partners"]);
    }

    #[test]
    fn a_token_without_a_valid_firm_is_refused() {
        for tenant in [None, Some("not-a-uuid"), Some(""), Some("1' OR '1'='1")] {
            assert!(
                verifier().verify(&sign(&claims(tenant), PRIVATE)).is_err(),
                "{tenant:?}"
            );
        }
    }

    #[test]
    fn a_token_signed_by_another_key_is_refused() {
        assert!(
            verifier()
                .verify(&sign(&claims(Some(TENANT)), OTHER_PRIVATE))
                .is_err()
        );
    }

    #[test]
    fn wrong_issuer_audience_or_expiry_is_refused() {
        for (field, value) in [
            ("iss", json!("https://evil.example")),
            ("aud", json!("someone-else")),
            ("exp", json!(now() - 3600)),
        ] {
            let mut bad = claims(Some(TENANT));
            bad[field] = value;
            assert!(verifier().verify(&sign(&bad, PRIVATE)).is_err(), "{field}");
        }
    }

    #[test]
    fn unsigned_and_hmac_tokens_are_refused() {
        use base64::Engine as _;
        let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
        let header = engine.encode(r#"{"alg":"none","typ":"JWT"}"#);
        let body = engine.encode(claims(Some(TENANT)).to_string());
        assert!(verifier().verify(&format!("{header}.{body}.")).is_err());

        // Signing with HS256 using the public key as the secret must not be accepted.
        let forged = encode(
            &Header::new(Algorithm::HS256),
            &claims(Some(TENANT)),
            &EncodingKey::from_secret(PUBLIC),
        )
        .unwrap();
        assert!(verifier().verify(&forged).is_err());
    }

    #[test]
    fn access_tokens_are_refused_even_when_otherwise_valid() {
        let mut access = claims(Some(TENANT));
        access["token_use"] = json!("access");
        assert!(verifier().verify(&sign(&access, PRIVATE)).is_err());
        access.as_object_mut().unwrap().remove("token_use");
        assert!(verifier().verify(&sign(&access, PRIVATE)).is_err());
    }

    #[test]
    fn keys_are_chosen_by_key_id_and_unknown_ids_are_refused() {
        use jsonwebtoken::jwk::Jwk;
        let jwk = Jwk::from_encoding_key(
            &EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
            Algorithm::RS256,
        )
        .unwrap();
        let mut jwk = serde_json::to_value(&jwk).unwrap();
        jwk["kid"] = json!("key-1");
        let jwks = json!({ "keys": [jwk] }).to_string();

        let verifier = TokenVerifier::new(ISSUER, AUDIENCE);
        assert!(
            verifier
                .verify(&sign(&claims(Some(TENANT)), PRIVATE))
                .is_err()
        );
        verifier.set_jwks(&jwks).unwrap();
        assert!(verifier.has_key("key-1"));

        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some("key-1".into());
        let signed = encode(
            &header,
            &claims(Some(TENANT)),
            &EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
        )
        .unwrap();
        assert!(verifier.verify(&signed).is_ok());

        header.kid = Some("someone-elses-key".into());
        let unknown = encode(
            &header,
            &claims(Some(TENANT)),
            &EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
        )
        .unwrap();
        assert!(verifier.verify(&unknown).is_err());
        assert!(verifier.set_jwks(r#"{"keys":[]}"#).is_err());
    }
}
