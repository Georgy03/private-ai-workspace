#![allow(dead_code)]
//! A stand-in for the parts of Amazon Cognito and the AWS parameter store that firm sign-in setup uses,
//! speaking their real JSON protocols so the real AWS clients are what the tests exercise.
//!
//! It copies the one behaviour that makes the real service dangerous: updating an app client RESETS every
//! setting the request leaves out. A test can therefore only pass if the code sends all of them back.

use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, Mutex},
};

use axum::{
    Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Map, Value, json};

#[derive(Default)]
pub struct SignInWorld {
    /// The desktop app's client, as the describe call returns it.
    pub client: Value,
    /// Providers by name: the request that made each.
    pub providers: BTreeMap<String, Value>,
    /// The parameter store: name to value.
    pub params: BTreeMap<String, String>,
    /// Every request, as "<Action> <what>", in order.
    pub calls: Vec<String>,
    /// Actions that answer with an error.
    pub fail: HashSet<String>,
    /// Invited people: username to firm ID.
    pub users: BTreeMap<String, String>,
    /// Group memberships: username to groups.
    pub groups: BTreeMap<String, Vec<String>>,
    /// Status and creation time (seconds since 1970) of users; anyone missing is a new outside-provider account.
    pub meta: BTreeMap<String, (String, i64)>,
    /// Users per page of a listing (0 means as many as asked for).
    pub page_size: usize,
    /// After the next listing, attach this user to this firm (a person finishing sign-up at that moment).
    pub attach_after_list: Option<(String, String)>,
}

pub type SignIn = Arc<Mutex<SignInWorld>>;

pub async fn cognito_and_ssm(world: SignIn) -> String {
    super::serve(Router::new().route("/", post(handle)).with_state(world)).await
}

/// An app client with something set in every field, so a setting lost in an update shows.
pub fn rich_client(pool: &str, client_id: &str) -> Value {
    json!({
        "UserPoolId": pool, "ClientId": client_id, "ClientName": "workspace-cloud-prod-desktop",
        "RefreshTokenValidity": 7, "AccessTokenValidity": 60, "IdTokenValidity": 60,
        "TokenValidityUnits": { "AccessToken": "minutes", "IdToken": "minutes", "RefreshToken": "days" },
        "ReadAttributes": ["email", "email_verified", "custom:tenant_id"], "WriteAttributes": ["email"],
        "ExplicitAuthFlows": ["ALLOW_REFRESH_TOKEN_AUTH", "ALLOW_USER_SRP_AUTH"],
        "SupportedIdentityProviders": ["COGNITO", "Google"],
        "CallbackURLs": ["http://127.0.0.1:53682/callback"], "LogoutURLs": ["http://127.0.0.1:53682/callback"],
        "DefaultRedirectURI": "http://127.0.0.1:53682/callback",
        "AllowedOAuthFlows": ["code"], "AllowedOAuthScopes": ["openid", "email", "profile"],
        "AllowedOAuthFlowsUserPoolClient": true,
        "PreventUserExistenceErrors": "ENABLED", "EnableTokenRevocation": true,
        "EnablePropagateAdditionalUserContextData": false, "AuthSessionValidity": 3,
        "RefreshTokenRotation": { "Feature": "ENABLED", "RetryGracePeriodSeconds": 20 },
    })
}

fn error(kind: &str, message: &str) -> Response {
    (StatusCode::BAD_REQUEST, [(header::CONTENT_TYPE, "application/x-amz-json-1.1")], json!({ "__type": kind, "message": message }).to_string()).into_response()
}

fn ok(body: Value) -> Response {
    ([(header::CONTENT_TYPE, "application/x-amz-json-1.1")], body.to_string()).into_response()
}

async fn handle(State(world): State<SignIn>, headers: HeaderMap, body: Bytes) -> Response {
    let target = headers.get("x-amz-target").and_then(|v| v.to_str().ok()).unwrap_or_default().to_string();
    let action = target.rsplit('.').next().unwrap_or_default().to_string();
    let request: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let mut w = world.lock().unwrap();
    let about = request["ProviderName"].as_str().or_else(|| request["Name"].as_str()).or_else(|| request["Username"].as_str()).unwrap_or_default().to_string();
    w.calls.push(format!("{action} {about}").trim().to_string());
    if w.fail.contains(&action) {
        return error("InternalErrorException", "made to fail by the test");
    }
    match action.as_str() {
        "AdminGetUser" => {
            let name = request["Username"].as_str().unwrap_or_default();
            match w.users.get(name) {
                Some(firm) => {
                    // An empty firm is a person who has signed in but belongs to none yet.
                    let address = if name.contains('@') { name.to_string() } else { format!("{name}@example.com") };
                    let mut attributes = vec![json!({ "Name": "email", "Value": address })];
                    if !firm.is_empty() {
                        attributes.push(json!({ "Name": "custom:tenant_id", "Value": firm }));
                    }
                    // An account named like "Google_123" was made by an outside sign-in, as in the real service.
                    if let Some((provider, subject)) = name.split_once('_') {
                        let identities = json!([{ "providerName": provider, "userId": subject, "providerType": provider }]);
                        attributes.push(json!({ "Name": "identities", "Value": identities.to_string() }));
                    }
                    ok(json!({ "Username": name, "Enabled": true, "UserStatus": "CONFIRMED", "UserAttributes": attributes }))
                }
                None => error("UserNotFoundException", "User does not exist."),
            }
        }
        "ListUsers" => {
            let names: Vec<String> = w.users.keys().cloned().collect();
            let start = request["PaginationToken"].as_str().and_then(|t| t.parse::<usize>().ok()).unwrap_or(0);
            let asked = request["Limit"].as_u64().unwrap_or(60) as usize;
            let size = if w.page_size > 0 { w.page_size.min(asked) } else { asked };
            let now = 1_900_000_000_i64;
            let page: Vec<Value> = names
                .iter()
                .skip(start)
                .take(size)
                .map(|name| {
                    let firm = &w.users[name];
                    let (status, created) = w.meta.get(name).cloned().unwrap_or(("EXTERNAL_PROVIDER".into(), now));
                    let mut attributes = vec![json!({ "Name": "email", "Value": format!("{name}@example.com") })];
                    if !firm.is_empty() {
                        attributes.push(json!({ "Name": "custom:tenant_id", "Value": firm }));
                    }
                    json!({ "Username": name, "UserStatus": status, "UserCreateDate": created as f64, "Attributes": attributes, "Enabled": true })
                })
                .collect();
            let mut reply = json!({ "Users": page });
            if start + size < names.len() {
                reply["PaginationToken"] = json!((start + size).to_string());
            }
            if let Some((user, firm)) = w.attach_after_list.take() {
                w.users.insert(user, firm);
            }
            ok(reply)
        }
        "AdminDeleteUser" => match w.users.remove(request["Username"].as_str().unwrap_or_default()) {
            Some(_) => ok(json!({})),
            None => error("UserNotFoundException", "User does not exist."),
        },
        "AdminUpdateUserAttributes" => {
            let name = request["Username"].as_str().unwrap_or_default().to_string();
            if !w.users.contains_key(&name) {
                return error("UserNotFoundException", "User does not exist.");
            }
            for attribute in request["UserAttributes"].as_array().into_iter().flatten() {
                match attribute["Name"].as_str() {
                    // The invited person's firm is immutable in the real pool: it can only be given when an account is made.
                    Some("custom:tenant_id") => return error("InvalidParameterException", "user.custom:tenant_id: Attribute cannot be updated."),
                    // The firm of someone who started their own is a separate attribute that can change.
                    Some("custom:firm_id") => {
                        w.users.insert(name.clone(), attribute["Value"].as_str().unwrap_or_default().to_string());
                    }
                    _ => {}
                }
            }
            ok(json!({}))
        }
        "AdminAddUserToGroup" => {
            let name = request["Username"].as_str().unwrap_or_default().to_string();
            let group = request["GroupName"].as_str().unwrap_or_default().to_string();
            w.groups.entry(name).or_default().push(group);
            ok(json!({}))
        }
        "DescribeUserPoolClient" => ok(json!({ "UserPoolClient": w.client })),
        "UpdateUserPoolClient" => {
            // Like the real service: whatever is not sent goes back to its default (here, absent).
            let mut next = Map::new();
            for key in ["UserPoolId", "ClientId"] {
                next.insert(key.into(), request[key].clone());
            }
            for (key, value) in request.as_object().into_iter().flatten() {
                if !matches!(key.as_str(), "UserPoolId" | "ClientId") && !value.is_null() {
                    next.insert(key.clone(), value.clone());
                }
            }
            w.client = Value::Object(next);
            ok(json!({ "UserPoolClient": w.client }))
        }
        "CreateIdentityProvider" => {
            if w.providers.contains_key(&about) {
                return error("DuplicateProviderException", "a provider with that name exists");
            }
            w.providers.insert(about.clone(), request.clone());
            ok(json!({ "IdentityProvider": { "ProviderName": about } }))
        }
        "DeleteIdentityProvider" => match w.providers.remove(&about) {
            Some(_) => ok(json!({})),
            None => error("ResourceNotFoundException", "no such provider"),
        },
        "PutParameter" => {
            let name = request["Name"].as_str().unwrap_or_default().to_string();
            if w.params.contains_key(&name) && request["Overwrite"] != json!(true) {
                return error("ParameterAlreadyExists", "the parameter exists");
            }
            w.params.insert(name, request["Value"].as_str().unwrap_or_default().to_string());
            ok(json!({ "Version": 1, "Tier": "Standard" }))
        }
        "DeleteParameter" => match w.params.remove(request["Name"].as_str().unwrap_or_default()) {
            Some(_) => ok(json!({})),
            None => error("ParameterNotFound", "no such parameter"),
        },
        "GetParametersByPath" => {
            let path = format!("{}/", request["Path"].as_str().unwrap_or_default().trim_end_matches('/'));
            let all: Vec<(&String, &String)> = w.params.iter().filter(|(name, _)| name.starts_with(&path)).collect();
            let start = request["NextToken"].as_str().and_then(|t| t.parse::<usize>().ok()).unwrap_or(0);
            let size = request["MaxResults"].as_u64().unwrap_or(10) as usize;
            let page: Vec<Value> = all.iter().skip(start).take(size).map(|(name, value)| json!({ "Name": name, "Type": "String", "Value": value, "Version": 1 })).collect();
            let mut reply = json!({ "Parameters": page });
            if start + size < all.len() {
                reply["NextToken"] = json!((start + size).to_string());
            }
            ok(reply)
        }
        _ => error("UnknownOperationException", "not supported by the stand-in"),
    }
}
