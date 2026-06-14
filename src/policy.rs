use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde_json::{Value, json};

use crate::config::PolicyProfile;

const CHROMIUM_MAX_POLICY_TEMPLATE: &str = include_str!("../policy/chromium_max_policies.json");

/// Ensure the managed Chromium policy file is present within the launcher profile root.
/// Returns the path to the managed policy file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyWriteAction {
    Created,
    Updated,
    Unchanged,
}

#[derive(Debug, Clone)]
pub struct PolicyWriteOutcome {
    pub path: PathBuf,
    pub action: PolicyWriteAction,
}

pub fn ensure_chromium_policy<P: AsRef<Path>>(
    profile_root: P,
    doh_template: &str,
    profile: PolicyProfile,
) -> Result<PolicyWriteOutcome> {
    let policy_dir = profile_root.as_ref().join("policies");
    fs::create_dir_all(&policy_dir)
        .with_context(|| format!("Unable to create policy directory {}", policy_dir.display()))?;

    let policy_path = policy_dir.join("chromium_max_policy.json");
    let mut policy: Value = serde_json::from_str(CHROMIUM_MAX_POLICY_TEMPLATE)
        .context("Embedded Chromium policy template is malformed")?;

    if let Some(map) = policy.as_object_mut() {
        map.insert(
            "DnsOverHttpsTemplates".into(),
            Value::String(doh_template.to_string()),
        );

        if profile == PolicyProfile::Default {
            map.insert("SafeBrowsingProtectionLevel".into(), json!(1));
            map.insert("PasswordManagerEnabled".into(), Value::Bool(true));
            map.insert("PasswordLeakDetectionEnabled".into(), Value::Bool(true));
            map.insert("SearchSuggestEnabled".into(), Value::Bool(true));
            map.insert("AlternateErrorPagesEnabled".into(), Value::Bool(true));
            map.insert("AutofillAddressEnabled".into(), Value::Bool(true));
            map.insert("AutofillCreditCardEnabled".into(), Value::Bool(true));
            map.insert("BlockExternalExtensions".into(), Value::Bool(false));
        }
    }

    let rendered = serde_json::to_string_pretty(&policy)?;
    let existed = policy_path.exists();
    let needs_write = match fs::read_to_string(&policy_path) {
        Ok(existing) => existing != rendered,
        Err(_) => true,
    };

    let action = if needs_write {
        fs::write(&policy_path, rendered).with_context(|| {
            format!(
                "Failed to write managed policy template to {}",
                policy_path.display()
            )
        })?;
        if existed {
            PolicyWriteAction::Updated
        } else {
            PolicyWriteAction::Created
        }
    } else {
        PolicyWriteAction::Unchanged
    };

    Ok(PolicyWriteOutcome {
        path: policy_path,
        action,
    })
}

pub fn load_policy(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path)
        .with_context(|| format!("Unable to read policy at {}", path.display()))?;
    let parsed = serde_json::from_str(&raw)
        .with_context(|| format!("Policy JSON is malformed at {}", path.display()))?;
    Ok(parsed)
}

#[derive(Debug, Clone, Default)]
pub struct PolicySummary {
    pub doh_mode: Option<String>,
    pub doh_template: Option<String>,
    pub safe_browsing_level: Option<i64>,
    pub password_manager_enabled: Option<bool>,
    pub leak_detection_enabled: Option<bool>,
    pub search_suggest_enabled: Option<bool>,
    pub block_external_extensions: Option<bool>,
    pub extension_forcelist: Vec<String>,
    pub remote_debugging_allowed: Option<bool>,
}

pub fn summarize_policy(value: &Value) -> PolicySummary {
    let mut summary = PolicySummary::default();
    if let Some(map) = value.as_object() {
        summary.doh_mode = map
            .get("DnsOverHttpsMode")
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        summary.doh_template = map
            .get("DnsOverHttpsTemplates")
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        summary.safe_browsing_level = map
            .get("SafeBrowsingProtectionLevel")
            .and_then(|v| v.as_i64());
        summary.password_manager_enabled =
            map.get("PasswordManagerEnabled").and_then(|v| v.as_bool());
        summary.leak_detection_enabled = map
            .get("PasswordLeakDetectionEnabled")
            .and_then(|v| v.as_bool());
        summary.search_suggest_enabled = map.get("SearchSuggestEnabled").and_then(|v| v.as_bool());
        summary.block_external_extensions =
            map.get("BlockExternalExtensions").and_then(|v| v.as_bool());
        summary.remote_debugging_allowed =
            map.get("RemoteDebuggingAllowed").and_then(|v| v.as_bool());
        if let Some(list) = map.get("ExtensionInstallForcelist")
            && let Some(array) = list.as_array()
        {
            summary.extension_forcelist = array
                .iter()
                .filter_map(|entry| entry.as_str().map(|s| s.to_string()))
                .collect();
        }
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const TEST_DOH: &str = "https://10.0.0.1:443/dns-query";

    #[test]
    fn embedded_template_is_valid_json() {
        let parsed: Value =
            serde_json::from_str(CHROMIUM_MAX_POLICY_TEMPLATE).expect("template parses");
        assert!(parsed.is_object());
    }

    #[test]
    fn template_uses_canonical_chromium_doh_keys() {
        // Chromium ignores unknown keys silently; the all-caps "DNS" spelling is
        // not a real policy. Guard against regressing to it.
        let map = serde_json::from_str::<Value>(CHROMIUM_MAX_POLICY_TEMPLATE).unwrap();
        let obj = map.as_object().unwrap();
        assert!(obj.contains_key("DnsOverHttpsMode"));
        assert!(!obj.contains_key("DNSOverHttpsMode"));
        assert!(!obj.contains_key("DNSOverHttpsTemplates"));
    }

    #[test]
    fn creates_then_reports_unchanged() {
        let dir = tempdir().unwrap();
        let first =
            ensure_chromium_policy(dir.path(), TEST_DOH, PolicyProfile::Hardened).unwrap();
        assert_eq!(first.action, PolicyWriteAction::Created);
        assert!(first.path.exists());

        let second =
            ensure_chromium_policy(dir.path(), TEST_DOH, PolicyProfile::Hardened).unwrap();
        assert_eq!(second.action, PolicyWriteAction::Unchanged);
        assert_eq!(first.path, second.path);
    }

    #[test]
    fn rewrites_when_doh_template_changes() {
        let dir = tempdir().unwrap();
        ensure_chromium_policy(dir.path(), TEST_DOH, PolicyProfile::Hardened).unwrap();
        let updated = ensure_chromium_policy(
            dir.path(),
            "https://192.168.0.1:443/dns-query",
            PolicyProfile::Hardened,
        )
        .unwrap();
        assert_eq!(updated.action, PolicyWriteAction::Updated);
    }

    #[test]
    fn hardened_profile_keeps_aggressive_defaults() {
        let dir = tempdir().unwrap();
        let outcome =
            ensure_chromium_policy(dir.path(), TEST_DOH, PolicyProfile::Hardened).unwrap();
        let summary = summarize_policy(&load_policy(&outcome.path).unwrap());
        assert_eq!(summary.doh_mode.as_deref(), Some("automatic"));
        assert_eq!(summary.doh_template.as_deref(), Some(TEST_DOH));
        assert_eq!(summary.safe_browsing_level, Some(2));
        assert_eq!(summary.password_manager_enabled, Some(false));
        assert_eq!(summary.search_suggest_enabled, Some(false));
    }

    #[test]
    fn default_profile_relaxes_convenience_settings() {
        let dir = tempdir().unwrap();
        let outcome =
            ensure_chromium_policy(dir.path(), TEST_DOH, PolicyProfile::Default).unwrap();
        let summary = summarize_policy(&load_policy(&outcome.path).unwrap());
        assert_eq!(summary.safe_browsing_level, Some(1));
        assert_eq!(summary.password_manager_enabled, Some(true));
        assert_eq!(summary.leak_detection_enabled, Some(true));
        assert_eq!(summary.search_suggest_enabled, Some(true));
        assert_eq!(summary.block_external_extensions, Some(false));
    }

    #[test]
    fn load_policy_rejects_malformed_json() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("broken.json");
        fs::write(&path, "{ not valid json").unwrap();
        assert!(load_policy(&path).is_err());
    }
}
