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
        // Configure the agent so non-2xx responses come back as `Ok(Response)`
        // rather than `Err`. Callers do their own status checking and have
        // direct access to the response body for both success and error cases.
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .build()
            .new_agent();
        UpCloudClient {
            auth_header,
            agent,
            base_url: DEFAULT_BASE_URL.to_string(),
        }
    }

    /// Issue an authenticated GET against `path` (relative to the API base URL).
    /// Returns the raw response for any HTTP status. Errors only on transport-
    /// level failures (network, TLS, etc.). Callers are responsible for status
    /// checking, body parsing, and any per-endpoint translation (e.g. 404 →
    /// domain-level "not found").
    pub fn get(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}{}", self.base_url, path);
        self.agent
            .get(&url)
            .header("Authorization", &self.auth_header)
            .call()
            .map_err(|e| format!("upcloud GET {path} failed: {e}"))
    }

    /// Issue an authenticated POST with a JSON body. Same status/error model
    /// as `get` — returns the raw response for any HTTP status.
    pub fn post<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}{}", self.base_url, path);
        self.agent
            .post(&url)
            .header("Authorization", &self.auth_header)
            .send_json(body)
            .map_err(|e| format!("upcloud POST {path} failed: {e}"))
    }

    /// Issue an authenticated DELETE. Path includes any query string the
    /// caller wants (caller is responsible for URL-encoding values if needed).
    /// Same status/error model as `get`.
    pub fn delete(&self, path: &str) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}{}", self.base_url, path);
        self.agent
            .delete(&url)
            .header("Authorization", &self.auth_header)
            .call()
            .map_err(|e| format!("upcloud DELETE {path} failed: {e}"))
    }

    /// Issue an authenticated PUT with a JSON body. Same status/error model
    /// as `get` — returns the raw response for any HTTP status.
    pub fn put<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}{}", self.base_url, path);
        self.agent
            .put(&url)
            .header("Authorization", &self.auth_header)
            .send_json(body)
            .map_err(|e| format!("upcloud PUT {path} failed: {e}"))
    }

    /// Issue an authenticated PATCH with a JSON body. Same status/error model
    /// as `get` — returns the raw response for any HTTP status.
    pub fn patch<B: serde::Serialize>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<ureq::http::Response<ureq::Body>, String> {
        let url = format!("{}{}", self.base_url, path);
        self.agent
            .patch(&url)
            .header("Authorization", &self.auth_header)
            .send_json(body)
            .map_err(|e| format!("upcloud PATCH {path} failed: {e}"))
    }
}
