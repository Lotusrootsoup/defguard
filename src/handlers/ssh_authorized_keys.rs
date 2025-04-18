use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use serde_json::json;
use sqlx::{query, Error as SqlxError, PgExecutor, PgPool};
use ssh_key::PublicKey;

use super::{user_for_admin_or_self, ApiResponse, ApiResult};
use crate::{
    appstate::AppState,
    auth::SessionInfo,
    db::{
        models::authentication_key::{AuthenticationKey, AuthenticationKeyType},
        Group, Id, User,
    },
    error::WebError,
};

#[derive(Deserialize, Serialize)]
pub(crate) struct AuthenticationKeyInfo {
    id: Id,
    name: Option<String>,
    key_type: AuthenticationKeyType,
    key: String,
    user_id: Id,
    yubikey_serial: Option<String>,
    yubikey_id: Option<i64>,
    yubikey_name: Option<String>,
}

impl AuthenticationKeyInfo {
    pub async fn find_by_user_id<'e, E>(executor: E, user_id: Id) -> Result<Vec<Self>, SqlxError>
    where
        E: PgExecutor<'e>,
    {
        let q_res = query!(
            "SELECT k.id key_id, k.name, k.key_type \"key_type: AuthenticationKeyType\", \
            k.key, k.user_id, k.yubikey_id, \
            y.name \"yubikey_name: Option<String>\", y.serial \"serial: Option<String>\" \
            FROM \"authentication_key\" k \
            LEFT JOIN \"yubikey\" y ON k.yubikey_id = y.id \
            WHERE k.user_id = $1",
            user_id
        )
        .fetch_all(executor)
        .await?;
        let res = q_res
            .iter()
            .map(|q| Self {
                id: q.key_id,
                key: q.key.clone(),
                key_type: q.key_type.clone(),
                user_id: q.user_id,
                name: q.name.clone(),
                yubikey_id: q.yubikey_id,
                yubikey_name: q.yubikey_name.clone(),
                yubikey_serial: q.serial.clone(),
            })
            .collect();

        Ok(res)
    }
}

async fn add_user_ssh_keys_to_list(pool: &PgPool, user: &User<Id>, ssh_keys: &mut Vec<String>) {
    let keys_result =
        AuthenticationKey::find_by_user_id(pool, user.id, Some(AuthenticationKeyType::Ssh)).await;

    if let Ok(authentication_keys) = keys_result {
        let mut keys: Vec<String> = authentication_keys
            .into_iter()
            .map(|item| item.key)
            .collect();
        ssh_keys.append(&mut keys);
    }
}

#[derive(Debug, Deserialize)]
pub struct SshKeysRequestParams {
    username: Option<String>,
    group: Option<String>,
}

/// Fetch public SSH keys for user
///
/// Meant to be used with `AuthorizedKeysCommand` config option in `sshd`.
/// Should always return a response to partially mitigate user enumeration.
/// Optional query params `username` and `group` are used for filtering users.
/// If no params are specified an empty response is returned.
pub async fn get_authorized_keys(
    params: Query<SshKeysRequestParams>,
    State(appstate): State<AppState>,
) -> Result<String, WebError> {
    info!("Fetching public SSH keys for {:?}", params);
    let mut ssh_keys: Vec<String> = Vec::new();

    // check if group filter was specified
    match &params.group {
        Some(group_name) => {
            // fetch group
            if let Some(group) = Group::find_by_name(&appstate.pool, group_name).await? {
                // check if user filter was specified
                if let Some(username) = &params.username {
                    debug!("Fetching SSH keys for user {username} in group {group_name}");
                    // fetch user
                    if let Some(user) = User::find_by_username(&appstate.pool, username).await? {
                        // check if user belongs to specified group
                        let members = group.member_usernames(&appstate.pool).await?;
                        if members.contains(&user.username) {
                            add_user_ssh_keys_to_list(&appstate.pool, &user, &mut ssh_keys).await;
                        } else {
                            debug!("User {username} is not a member of group {group_name}",);
                        }
                    } else {
                        debug!("Specified user does not exist");
                    }
                } else {
                    debug!("Fetching SSH keys for all users in group {group_name}");
                    // fetch all users in group
                    let users = group.members(&appstate.pool).await?;
                    for user in users {
                        add_user_ssh_keys_to_list(&appstate.pool, &user, &mut ssh_keys).await;
                    }
                }
            } else {
                debug!("Specified group does not exist");
            }
        }
        None => {
            // check if user filter was specified
            if let Some(username) = &params.username {
                debug!("Fetching SSH keys for user {username}");
                // fetch user
                if let Some(user) = User::find_by_username(&appstate.pool, username).await? {
                    add_user_ssh_keys_to_list(&appstate.pool, &user, &mut ssh_keys).await;
                } else {
                    debug!("Specified user does not exist");
                }
            }
        }
    }

    // concatenate all keys into a response
    Ok(ssh_keys.join("\n"))
}

#[derive(Deserialize, Serialize, Debug)]
pub struct AddAuthenticationKeyData {
    key: String,
    name: String,
    key_type: AuthenticationKeyType,
}

pub async fn add_authentication_key(
    State(appstate): State<AppState>,
    session: SessionInfo,
    Path(username): Path<String>,
    Json(data): Json<AddAuthenticationKeyData>,
) -> ApiResult {
    debug!(
        "Adding authentication key of type {:?} for user {username}",
        data.key_type,
    );

    // authorize request
    let user = user_for_admin_or_self(&appstate.pool, &session, &username).await?;

    let trimmed_key = data.key.trim_end_matches(['\n', '\r']);

    // verify key
    match data.key_type {
        AuthenticationKeyType::Ssh => {
            let parsed = trimmed_key.parse::<PublicKey>();
            if parsed.is_err() {
                error!("User {username} tried to insert invalid SSH key: {data:?}");
                return Err(WebError::BadRequest("SSH key failed verification.".into()));
            }
        }
        // FIXME: verify GPG key
        AuthenticationKeyType::Gpg => {}
    }

    // check if exists
    let exists_res = query!(
        "SELECT COUNT(1) FROM \"authentication_key\" WHERE user_id = $1 AND key = $2",
        user.id,
        trimmed_key,
    )
    .fetch_one(&appstate.pool)
    .await?;
    if exists_res.count == Some(1) {
        error!("User {username} tried to insert existing key: {data:?}");
        return Err(WebError::BadRequest("Key already exists.".into()));
    }

    AuthenticationKey::new(
        user.id,
        trimmed_key.to_string(),
        Some(data.name.clone()),
        data.key_type.clone(),
        None,
    )
    .save(&appstate.pool)
    .await?;

    info!(
        "Added new key \"{}\" of type {:?} for user {username}",
        data.name, data.key_type
    );

    Ok(ApiResponse {
        json: json!({}),
        status: StatusCode::CREATED,
    })
}

// GET on user, returns AuthenticationKeyInfo vector in JSON
pub async fn fetch_authentication_keys(
    State(appstate): State<AppState>,
    Path(username): Path<String>,
    session: SessionInfo,
) -> ApiResult {
    let user = user_for_admin_or_self(&appstate.pool, &session, &username).await?;
    let keys_info = AuthenticationKeyInfo::find_by_user_id(&appstate.pool, user.id).await?;

    Ok(ApiResponse {
        json: json!(keys_info),
        status: StatusCode::OK,
    })
}

pub async fn delete_authentication_key(
    State(appstate): State<AppState>,
    session: SessionInfo,
    Path((username, key_id)): Path<(String, i64)>,
) -> ApiResult {
    let user = user_for_admin_or_self(&appstate.pool, &session, &username).await?;
    if let Some(key) = AuthenticationKey::find_by_id(&appstate.pool, key_id).await? {
        if !session.is_admin && user.id != key.user_id {
            return Err(WebError::Forbidden(String::new()));
        }
        key.delete(&appstate.pool).await?;
    } else {
        error!("Key with id {} not found", key_id);
        return Err(WebError::BadRequest("Key not found".into()));
    }

    Ok(ApiResponse {
        json: json!({}),
        status: StatusCode::OK,
    })
}

#[derive(Debug, Deserialize, Clone)]
pub struct RenameRequest {
    name: String,
}

pub async fn rename_authentication_key(
    State(appstate): State<AppState>,
    session: SessionInfo,
    Path((username, key_id)): Path<(String, i64)>,
    Json(data): Json<RenameRequest>,
) -> ApiResult {
    let user = user_for_admin_or_self(&appstate.pool, &session, &username).await?;
    if let Some(mut key) = AuthenticationKey::find_by_id(&appstate.pool, key_id).await? {
        if key.yubikey_id.is_some() {
            warn!(
                "User {} tried to rename authentication key instead of yubikey",
                username
            );
            return Err(WebError::BadRequest("Rename yubikey instead.".into()));
        }
        if !session.is_admin && user.id != key.user_id {
            warn!(
                "User {} tried to rename key ({}) of another user with id {}",
                username, key_id, key.user_id
            );
            return Err(WebError::Forbidden(String::new()));
        }
        key.name = Some(data.name);
        key.save(&appstate.pool).await?;
    } else {
        error!(
            "User {} tried to rename non-existing key with id {}",
            username, key_id
        );
        return Err(WebError::ObjectNotFound(String::new()));
    }

    Ok(ApiResponse {
        json: json!({}),
        status: StatusCode::OK,
    })
}
