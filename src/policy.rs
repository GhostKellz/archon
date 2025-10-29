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

#[derive(Debug, Clone)]
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

impl Default for PolicySummary {
    fn default() -> Self {
        Self {
            doh_mode: None,
            doh_template: None,
            safe_browsing_level: None,
            password_manager_enabled: None,
            leak_detection_enabled: None,
            search_suggest_enabled: None,
            block_external_extensions: None,
            extension_forcelist: Vec::new(),
            remote_debugging_allowed: None,
        }
    }
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
        if let Some(list) = map.get("ExtensionInstallForcelist") {
            if let Some(array) = list.as_array() {
                summary.extension_forcelist = array
                    .iter()
                    .filter_map(|entry| entry.as_str().map(|s| s.to_string()))
                    .collect();
            }
        }
    }
    summary
}
