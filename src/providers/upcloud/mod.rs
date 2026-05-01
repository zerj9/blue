pub mod client;
pub mod storage;

use std::collections::HashMap;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use serde_json::Value;

use crate::config::ProviderDef;
use crate::provider::{DataSourceType, ProviderInstance, Providers, ResourceType};

use client::UpCloudClient;
use storage::UpCloudStorageDataSource;

pub struct UpCloudProvider {
    storage_data_source: UpCloudStorageDataSource,
}

impl ProviderInstance for UpCloudProvider {
    fn resource_type(&self, _name: &str) -> Option<&dyn ResourceType> {
        None
    }

    fn data_source_type(&self, name: &str) -> Option<&dyn DataSourceType> {
        match name {
            "storage" => Some(&self.storage_data_source),
            _ => None,
        }
    }
}

/// Resolve UpCloud credentials from a provider config block into an
/// `Authorization` header value. Accepts either a token (`token` /
/// `token_env`) or username/password (`username` / `username_env` and
/// `password` / `password_env`). The two forms are mutually exclusive.
fn parse_credentials(config: &HashMap<String, Value>) -> Result<String, String> {
    let token = read_optional_string_field(config, "token")?;
    let username = read_optional_string_field(config, "username")?;
    let password = read_optional_string_field(config, "password")?;

    match (token, username, password) {
        (Some(_), Some(_), _) | (Some(_), _, Some(_)) => Err(
            "specify either 'token' or 'username'/'password', not both".to_string(),
        ),
        (Some(t), None, None) => Ok(format!("Bearer {t}")),
        (None, Some(u), Some(p)) => {
            let encoded = BASE64_STANDARD.encode(format!("{u}:{p}"));
            Ok(format!("Basic {encoded}"))
        }
        (None, Some(_), None) => Err("'username' is set but 'password' is missing".to_string()),
        (None, None, Some(_)) => Err("'password' is set but 'username' is missing".to_string()),
        (None, None, None) => Err(
            "missing credentials: provide 'token'/'token_env' or 'username'/'password'"
                .to_string(),
        ),
    }
}

/// Read `<field>` directly or `<field>_env` (env var name).
/// Returns `Ok(None)` if neither form is present. Errors on conflicts,
/// non-string values, and unset env var references.
fn read_optional_string_field(
    config: &HashMap<String, Value>,
    field: &str,
) -> Result<Option<String>, String> {
    let env_field = format!("{field}_env");
    let direct = config.get(field);
    let env_ref = config.get(&env_field);

    match (direct, env_ref) {
        (Some(_), Some(_)) => Err(format!(
            "specify either '{field}' or '{env_field}', not both"
        )),
        (Some(value), None) => value
            .as_str()
            .map(|s| Some(s.to_string()))
            .ok_or_else(|| format!("field '{field}' must be a string")),
        (None, Some(value)) => {
            let var_name = value
                .as_str()
                .ok_or_else(|| format!("field '{env_field}' must be a string"))?;
            std::env::var(var_name).map(Some).map_err(|_| {
                format!(
                    "environment variable '{var_name}' (referenced by '{env_field}') is not set"
                )
            })
        }
        (None, None) => Ok(None),
    }
}

pub fn register(
    providers: &mut Providers,
    instance_name: &str,
    def: &ProviderDef,
) -> Result<(), String> {
    let auth_header = parse_credentials(&def.config)
        .map_err(|e| format!("provider '{instance_name}': {e}"))?;
    let client = Arc::new(UpCloudClient::new(auth_header));
    let storage_data_source = UpCloudStorageDataSource::new(client);
    providers.register(
        instance_name,
        Box::new(UpCloudProvider { storage_data_source }),
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn cfg(pairs: &[(&str, &str)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), Value::String(v.to_string())))
            .collect()
    }

    #[test]
    fn parse_credentials_basic_from_direct_strings() {
        let config = cfg(&[("username", "alice"), ("password", "secret")]);
        let header = parse_credentials(&config).unwrap();
        // "Basic " + base64("alice:secret")
        let expected = format!("Basic {}", BASE64_STANDARD.encode("alice:secret"));
        assert_eq!(header, expected);
    }

    #[test]
    fn parse_credentials_basic_from_env() {
        let user_var = format!("BLUE_TEST_USER_{}", Uuid::new_v4().simple());
        let pass_var = format!("BLUE_TEST_PASS_{}", Uuid::new_v4().simple());
        unsafe {
            std::env::set_var(&user_var, "bob");
            std::env::set_var(&pass_var, "topsecret");
        }

        let config = cfg(&[("username_env", &user_var), ("password_env", &pass_var)]);
        let header = parse_credentials(&config).unwrap();
        let expected = format!("Basic {}", BASE64_STANDARD.encode("bob:topsecret"));
        assert_eq!(header, expected);

        unsafe {
            std::env::remove_var(&user_var);
            std::env::remove_var(&pass_var);
        }
    }

    #[test]
    fn parse_credentials_token_from_direct_string() {
        let config = cfg(&[("token", "ucat_01DQE3AJDEBFEKECFM558TGH2F")]);
        let header = parse_credentials(&config).unwrap();
        assert_eq!(header, "Bearer ucat_01DQE3AJDEBFEKECFM558TGH2F");
    }

    #[test]
    fn parse_credentials_token_from_env() {
        let var = format!("BLUE_TEST_TOKEN_{}", Uuid::new_v4().simple());
        unsafe {
            std::env::set_var(&var, "ucat_xyz");
        }

        let config = cfg(&[("token_env", &var)]);
        let header = parse_credentials(&config).unwrap();
        assert_eq!(header, "Bearer ucat_xyz");

        unsafe {
            std::env::remove_var(&var);
        }
    }

    #[test]
    fn parse_credentials_token_and_username_errors() {
        let config = cfg(&[("token", "ucat_x"), ("username", "alice"), ("password", "y")]);
        let err = parse_credentials(&config).unwrap_err();
        assert!(err.contains("not both"), "got: {err}");
        assert!(err.contains("token"), "got: {err}");
    }

    #[test]
    fn parse_credentials_username_without_password_errors() {
        let config = cfg(&[("username", "alice")]);
        let err = parse_credentials(&config).unwrap_err();
        assert!(err.contains("password"), "got: {err}");
        assert!(err.contains("missing"), "got: {err}");
    }

    #[test]
    fn parse_credentials_password_without_username_errors() {
        let config = cfg(&[("password", "x")]);
        let err = parse_credentials(&config).unwrap_err();
        assert!(err.contains("username"), "got: {err}");
        assert!(err.contains("missing"), "got: {err}");
    }

    #[test]
    fn parse_credentials_no_credentials_errors() {
        let config = cfg(&[]);
        let err = parse_credentials(&config).unwrap_err();
        assert!(err.contains("missing credentials"), "got: {err}");
        assert!(err.contains("token"), "got: {err}");
    }

    #[test]
    fn parse_credentials_both_field_forms_errors() {
        let config = cfg(&[
            ("username", "alice"),
            ("username_env", "WHATEVER"),
            ("password", "x"),
        ]);
        let err = parse_credentials(&config).unwrap_err();
        assert!(err.contains("not both"), "got: {err}");
    }

    #[test]
    fn parse_credentials_env_unset_errors() {
        let var_name = format!("BLUE_TEST_UNSET_{}", Uuid::new_v4().simple());
        let config = cfg(&[("token_env", &var_name)]);
        let err = parse_credentials(&config).unwrap_err();
        assert!(err.contains(&var_name), "got: {err}");
        assert!(err.contains("not set"), "got: {err}");
    }

    #[test]
    fn parse_credentials_non_string_value_errors() {
        let mut config = HashMap::new();
        config.insert("token".to_string(), Value::Number(42.into()));
        let err = parse_credentials(&config).unwrap_err();
        assert!(err.contains("string"), "got: {err}");
    }

    #[test]
    fn register_succeeds_with_valid_credentials() {
        let mut providers = Providers::new();
        let def = ProviderDef {
            provider_type: "upcloud".to_string(),
            source: None,
            config: cfg(&[("username", "alice"), ("password", "secret")]),
        };
        register(&mut providers, "upcloud-eu", &def).unwrap();
    }

    #[test]
    fn register_wraps_error_with_instance_name() {
        let mut providers = Providers::new();
        let def = ProviderDef {
            provider_type: "upcloud".to_string(),
            source: None,
            config: cfg(&[]),
        };
        let err = register(&mut providers, "upcloud-broken", &def).unwrap_err();
        assert!(err.contains("upcloud-broken"), "got: {err}");
    }
}
