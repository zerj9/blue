use serde::de::DeserializeOwned;

const DEFAULT_BASE_URL: &str = "https://api.upcloud.com/1.3";

pub struct UpCloudClient {
    auth_header: String,
    agent: ureq::Agent,
    base_url: String,
}

impl UpCloudClient {
    /// Construct a client from a pre-built `Authorization` header value
    /// (e.g. `"Basic <base64>"` or `"Bearer <token>"`). The choice of auth
    /// scheme lives in the caller (see `parse_credentials` in `mod.rs`).
    pub fn new(auth_header: String) -> Self {
        UpCloudClient {
            auth_header,
            agent: ureq::Agent::new_with_defaults(),
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Issue an authenticated GET against `path` (relative to the API base URL)
    /// and deserialize the JSON response into `T`.
    pub fn get<T: DeserializeOwned>(&self, path: &str) -> Result<T, String> {
        let url = format!("{}{}", self.base_url, path);
        let mut response = self
            .agent
            .get(&url)
            .header("Authorization", &self.auth_header)
            .call()
            .map_err(|e| format!("upcloud GET {path} failed: {e}"))?;

        response
            .body_mut()
            .read_json()
            .map_err(|e| format!("upcloud GET {path} response parse failed: {e}"))
    }
}
