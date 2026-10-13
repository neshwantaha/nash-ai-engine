//! Security primitives: destination validation (SSRF) and secret redaction.

pub mod redact;
pub mod url_guard;

pub use redact::{
    is_sensitive_header, is_sensitive_key, redact_headers, redact_json, redact_secret, redact_text,
};
pub use url_guard::{is_loopback_host, is_metadata_host, is_metadata_ip, is_private_ip, SecurityPolicy};
