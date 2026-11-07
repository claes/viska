use crate::{Result, MAX_PAYLOAD};
use serde::Deserialize;
use std::{fs, path::PathBuf};

#[derive(Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Authorization {
    pub store_users: Vec<String>,
    pub type_users: Vec<String>,
    pub clear_users: Vec<String>,
    pub status_users: Vec<String>,
}
#[derive(Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub keyboard_layout: String,
    pub ttl_seconds: u64,
    pub max_payload_bytes: usize,
    pub authorization: Authorization,
    pub submission_socket: PathBuf,
    pub control_socket: PathBuf,
    pub backend: Backend,
}
#[derive(Clone, Copy, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Uinput,
    Mock,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            keyboard_layout: "us".into(),
            ttl_seconds: 45,
            max_payload_bytes: MAX_PAYLOAD,
            authorization: Authorization::default(),
            submission_socket: "/run/viska/submit.sock".into(),
            control_socket: "/run/viska/control.sock".into(),
            backend: Backend::Uinput,
        }
    }
}
impl Config {
    pub fn parse(data: &[u8]) -> Result<Self> {
        // Deserialize directly into typed structs: duplicate fields are errors.
        let c: Self = serde_json::from_slice(data).map_err(|_| "invalid JSON configuration")?;
        if !(1..=300).contains(&c.ttl_seconds) || !(1..=MAX_PAYLOAD).contains(&c.max_payload_bytes)
        {
            return Err("configuration limits out of range");
        }
        crate::layout::Layout::new(&c.keyboard_layout)?;
        if c.submission_socket == c.control_socket {
            return Err("endpoints must be distinct");
        }
        for p in [&c.submission_socket, &c.control_socket] {
            if !p.is_absolute() || p.as_os_str().len() > 100 {
                return Err("invalid endpoint path");
            }
        }
        for list in [
            &c.authorization.store_users,
            &c.authorization.type_users,
            &c.authorization.clear_users,
            &c.authorization.status_users,
        ] {
            if list.len() > 64
                || list
                    .iter()
                    .any(|s| s.is_empty() || s.len() > 256 || s.contains('\0'))
            {
                return Err("invalid account allowlist");
            }
        }
        Ok(c)
    }
    pub fn load(path: &str) -> Result<Self> {
        use std::io::Read;
        let f = fs::File::open(path).map_err(|_| "cannot open configuration")?;
        let mut bytes = Vec::new();
        f.take(65_537)
            .read_to_end(&mut bytes)
            .map_err(|_| "cannot read configuration")?;
        if bytes.len() > 65_536 {
            return Err("configuration too large");
        }
        Self::parse(&bytes)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strict_json() {
        assert_eq!(Config::parse(b"{}").unwrap().keyboard_layout, "us");
        for bad in [
            r#"{"ttl_seconds":0}"#,
            r#"{"max_payload_bytes":1048577}"#,
            r#"{"keyboard_layout":"xx"}"#,
            r#"{"unexpected":true}"#,
            r#"{"ttl_seconds":1,"ttl_seconds":2}"#,
            r#"{"authorization":{"type_users":[],"type_users":[]}}"#,
            r#"{"authorization":{"type_users":[42]}}"#,
        ] {
            assert!(Config::parse(bad.as_bytes()).is_err());
        }
        assert_eq!(
            Config::parse(br#"{"keyboard_layout":"se"}"#)
                .unwrap()
                .keyboard_layout,
            "se"
        );
    }
}
