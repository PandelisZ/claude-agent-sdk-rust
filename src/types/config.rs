use serde::{Deserialize, Serialize};

use super::{PermissionMode, SettingSource};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SkillsConfig {
    All,
    Names(Vec<String>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum SessionStoreFlushMode {
    #[default]
    Batched,
    Eager,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxNetworkConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub denied_domains: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_managed_domains_only: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_unix_sockets: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_all_unix_sockets: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_local_binding: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_mach_lookup: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub http_proxy_port: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub socks_proxy_port: Option<i32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxIgnoreViolations {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SandboxSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_allow_bash_if_sandboxed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excluded_commands: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_unsandboxed_commands: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<SandboxNetworkConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_violations: Option<SandboxIgnoreViolations>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub enable_weaker_nested_sandbox: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDefinition {
    pub description: String,
    pub prompt: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disallowed_tools: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skills: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<SettingSource>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<Vec<serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub background: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<PermissionMode>,
}

// ---------------------------------------------------------------------------
// Permission update vocabulary (parity with the Python `PermissionBehavior`,
// `PermissionUpdateDestination` and `PermissionUpdate.type` literals).
//
// Each enum keeps an `Other(String)` fallback so values from a newer CLI
// round-trip instead of failing to deserialize.
// ---------------------------------------------------------------------------

macro_rules! open_string_enum {
    (
        $(#[$meta:meta])*
        $name:ident { $($variant:ident => $wire:literal),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash)]
        pub enum $name {
            $($variant,)*
            /// A value this SDK version does not know, kept verbatim.
            Other(String),
        }

        impl $name {
            /// The wire string for this value.
            pub fn as_str(&self) -> &str {
                match self {
                    $($name::$variant => $wire,)*
                    $name::Other(value) => value,
                }
            }
        }

        impl From<&str> for $name {
            fn from(value: &str) -> Self {
                match value {
                    $($wire => $name::$variant,)*
                    other => $name::Other(other.to_string()),
                }
            }
        }

        impl From<String> for $name {
            fn from(value: String) -> Self {
                $name::from(value.as_str())
            }
        }

        impl From<$name> for String {
            fn from(value: $name) -> Self {
                value.as_str().to_string()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: serde::Serializer>(
                &self,
                serializer: S,
            ) -> std::result::Result<S::Ok, S::Error> {
                serializer.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: serde::Deserializer<'de>>(
                deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                Ok($name::from(String::deserialize(deserializer)?))
            }
        }
    };
}

open_string_enum! {
    /// Permission rule behavior (`"allow" | "deny" | "ask"`).
    PermissionBehavior {
        Allow => "allow",
        Deny => "deny",
        Ask => "ask",
    }
}

open_string_enum! {
    /// Where a permission update applies and is kept. `None` on a
    /// [`PermissionUpdate`](super::PermissionUpdate) is sent as `"session"`.
    PermissionUpdateDestination {
        UserSettings => "userSettings",
        ProjectSettings => "projectSettings",
        LocalSettings => "localSettings",
        Session => "session",
    }
}

open_string_enum! {
    /// The kind of a permission update (`PermissionUpdate.type`).
    PermissionUpdateType {
        AddRules => "addRules",
        ReplaceRules => "replaceRules",
        RemoveRules => "removeRules",
        SetMode => "setMode",
        AddDirectories => "addDirectories",
        RemoveDirectories => "removeDirectories",
    }
}

impl super::PermissionUpdate {
    /// Convert to the control-protocol dict format, validating it first.
    ///
    /// Mirrors upstream `PermissionUpdate.to_dict` (#1502). Claude Code rejects
    /// an update that lacks a field its type needs, or whose `ruleContent` is
    /// `null`, and then ignores every update in the same response. So this:
    ///
    /// - leaves `ruleContent` out when `rule_content` is `None`;
    /// - sends `"session"` when `destination` is `None`;
    /// - emits only the fields the `type` uses;
    /// - errors when a field the type needs is `None` (`rules` and `behavior`
    ///   for `addRules`/`replaceRules`/`removeRules`, `mode` for `setMode`,
    ///   `directories` for `addDirectories`/`removeDirectories`), or when the
    ///   type is none of those six.
    pub fn to_dict(&self) -> crate::error::Result<serde_json::Value> {
        use crate::error::ClaudeSDKError;
        let kind = self.r#type.as_str();
        let mut result = serde_json::Map::new();
        result.insert("type".to_string(), serde_json::json!(kind));
        result.insert(
            "destination".to_string(),
            serde_json::json!(self
                .destination
                .as_deref()
                .filter(|d| !d.is_empty())
                .unwrap_or("session")),
        );

        match PermissionUpdateType::from(kind) {
            PermissionUpdateType::AddRules
            | PermissionUpdateType::ReplaceRules
            | PermissionUpdateType::RemoveRules => {
                let Some(rules) = &self.rules else {
                    return Err(ClaudeSDKError::Other(format!(
                        "PermissionUpdate(type='{kind}') has no rules. \
                         Set rules to a list of PermissionRuleValue."
                    )));
                };
                let Some(behavior) = &self.behavior else {
                    return Err(ClaudeSDKError::Other(format!(
                        "PermissionUpdate(type='{kind}') has no behavior. \
                         Set behavior to 'allow', 'deny' or 'ask'."
                    )));
                };
                let wire_rules: Vec<serde_json::Value> = rules
                    .iter()
                    .map(|rule| {
                        let mut wire_rule = serde_json::Map::new();
                        wire_rule.insert("toolName".to_string(), serde_json::json!(rule.tool_name));
                        if let Some(content) = &rule.rule_content {
                            wire_rule.insert("ruleContent".to_string(), serde_json::json!(content));
                        }
                        serde_json::Value::Object(wire_rule)
                    })
                    .collect();
                result.insert("rules".to_string(), serde_json::Value::Array(wire_rules));
                result.insert("behavior".to_string(), serde_json::json!(behavior));
            }
            PermissionUpdateType::SetMode => {
                let Some(mode) = &self.mode else {
                    return Err(ClaudeSDKError::Other(
                        "PermissionUpdate(type='setMode') has no mode. \
                         Set mode to a permission mode such as 'acceptEdits'."
                            .to_string(),
                    ));
                };
                result.insert("mode".to_string(), serde_json::to_value(mode)?);
            }
            PermissionUpdateType::AddDirectories | PermissionUpdateType::RemoveDirectories => {
                let Some(directories) = &self.directories else {
                    return Err(ClaudeSDKError::Other(format!(
                        "PermissionUpdate(type='{kind}') has no directories. \
                         Set directories to a list of paths."
                    )));
                };
                result.insert("directories".to_string(), serde_json::json!(directories));
            }
            PermissionUpdateType::Other(_) => {
                return Err(ClaudeSDKError::Other(format!(
                    "PermissionUpdate has type '{kind}', which this version of the SDK \
                     cannot send. Use one of addRules, replaceRules, removeRules, setMode, \
                     addDirectories or removeDirectories, or upgrade the SDK if this update \
                     came from Claude Code."
                )));
            }
        }

        Ok(serde_json::Value::Object(result))
    }

    /// Construct from the control-protocol dict format (inverse of
    /// [`to_dict`](Self::to_dict)).
    ///
    /// Lenient like upstream `PermissionUpdate.from_dict`: only `type` (and
    /// `toolName` on each rule) is required; every other field is taken as-is
    /// and `destination` stays `None` when absent. A `mode` this SDK version
    /// does not know is dropped to `None` (the field is a typed
    /// `PermissionMode`).
    pub fn from_dict(data: &serde_json::Value) -> crate::error::Result<Self> {
        use crate::error::ClaudeSDKError;
        let kind = data.get("type").and_then(|v| v.as_str()).ok_or_else(|| {
            ClaudeSDKError::Other("PermissionUpdate dict has no string 'type'".to_string())
        })?;
        let rules = match data.get("rules") {
            None | Some(serde_json::Value::Null) => None,
            Some(serde_json::Value::Array(items)) => Some(
                items
                    .iter()
                    .map(|rule| {
                        let tool_name =
                            rule.get("toolName")
                                .and_then(|v| v.as_str())
                                .ok_or_else(|| {
                                    ClaudeSDKError::Other(
                                        "PermissionUpdate rule has no string 'toolName'"
                                            .to_string(),
                                    )
                                })?;
                        Ok(super::PermissionRuleValue {
                            tool_name: tool_name.to_string(),
                            rule_content: rule
                                .get("ruleContent")
                                .and_then(|v| v.as_str())
                                .map(String::from),
                        })
                    })
                    .collect::<crate::error::Result<Vec<_>>>()?,
            ),
            Some(_) => {
                return Err(ClaudeSDKError::Other(
                    "PermissionUpdate 'rules' is not a list".to_string(),
                ))
            }
        };
        let string_field = |key: &str| data.get(key).and_then(|v| v.as_str()).map(String::from);
        Ok(Self {
            r#type: kind.to_string(),
            rules,
            behavior: string_field("behavior"),
            mode: data
                .get("mode")
                .and_then(|v| serde_json::from_value(v.clone()).ok()),
            directories: data.get("directories").and_then(|v| {
                v.as_array().map(|items| {
                    items
                        .iter()
                        .filter_map(|item| item.as_str().map(String::from))
                        .collect()
                })
            }),
            destination: string_field("destination"),
        })
    }
}

/// Convert the permission updates of a `can_use_tool` allow result.
///
/// Mirrors upstream `_permission_updates_to_wire`: returns `None`, and logs a
/// warning, if any update cannot be converted. The updates are sent all
/// together or not at all, because the one that failed may be a restriction
/// the others were meant to come with. An empty slice yields `Some([])`.
pub(crate) fn permission_updates_to_wire(
    updates: &[super::PermissionUpdate],
) -> Option<Vec<serde_json::Value>> {
    let mut wire = Vec::with_capacity(updates.len());
    for (index, update) in updates.iter().enumerate() {
        match update.to_dict() {
            Ok(value) => wire.push(value),
            Err(error) => {
                tracing::warn!(
                    "The can_use_tool callback allowed the tool call, but none of its \
                     permission updates were applied, because updated_permissions[{index}] \
                     cannot be sent: {error}"
                );
                return None;
            }
        }
    }
    Some(wire)
}

/// Parse `permission_suggestions` from a `can_use_tool` control request with
/// [`PermissionUpdate::from_dict`](super::PermissionUpdate::from_dict)
/// semantics. Upstream raises (and answers the request with an error) when a
/// suggestion cannot be parsed.
pub(crate) fn permission_suggestions_from_values(
    values: &[serde_json::Value],
) -> crate::error::Result<Vec<super::PermissionUpdate>> {
    values
        .iter()
        .map(super::PermissionUpdate::from_dict)
        .collect()
}

// ---------------------------------------------------------------------------
// can_use_tool shadowing advisory.
// ---------------------------------------------------------------------------

/// `can_use_tool` is set but some tool calls are auto-approved before it runs.
///
/// Mirrors the upstream Python `CanUseToolShadowedWarning` (a `UserWarning`
/// subclass). The SDK reports it through `tracing::warn!` when connecting or
/// starting a query; it is advisory only, since shadowing can be intentional.
/// Compute it ahead of time with [`can_use_tool_shadowed_warning`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanUseToolShadowedWarning {
    pub message: String,
}

impl std::fmt::Display for CanUseToolShadowedWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// The tool an `allowed_tools` entry allows outright, if any.
///
/// Mirrors the CLI's rule parser: an entry allows a whole tool when it has no
/// `(...)` specifier (`"Read"`), or when the specifier is empty or a lone
/// wildcard (`"Read()"`, `"Read(*)"`). A real specifier (`"Bash(ls:*)"`) only
/// allows matching invocations. Malformed entries match nothing.
pub(crate) fn whole_tool_allowed(entry: &str) -> Option<&str> {
    if entry.trim().is_empty() {
        return None;
    }
    let Some(open_index) = entry.find('(') else {
        return Some(entry);
    };
    if open_index == 0 || !entry.ends_with(')') {
        return None;
    }
    let inner = &entry[open_index + 1..entry.len() - 1];
    matches!(inner, "" | "*").then(|| &entry[..open_index])
}

fn shadowed_warning_message(
    permission_mode: Option<PermissionMode>,
    allowed_tools: &[String],
) -> Option<String> {
    if permission_mode == Some(PermissionMode::BypassPermissions) {
        return Some(
            "can_use_tool will not be invoked: permission_mode 'bypassPermissions' \
             auto-approves every tool call (except explicit deny rules) before the \
             callback is consulted. To gate every tool call, use a PreToolUse hook instead."
                .to_string(),
        );
    }
    let mut shadowed: Vec<&str> = Vec::new();
    for tool in allowed_tools.iter().filter_map(|e| whole_tool_allowed(e)) {
        if !shadowed.contains(&tool) {
            shadowed.push(tool);
        }
    }
    if shadowed.is_empty() {
        return None;
    }
    Some(format!(
        "can_use_tool will not be invoked for: {}. An allowed_tools entry that allows a \
         whole tool auto-approves it before the callback is consulted. To gate every tool \
         call, use a PreToolUse hook; or narrow the entry so calls fall through to \
         can_use_tool. Allow rules from settings files can also shadow the callback but \
         are not visible here.",
        shadowed.join(", ")
    ))
}

/// The [`CanUseToolShadowedWarning`] these options would produce, if any.
///
/// Returns `None` unless `can_use_tool` is set. With
/// `permission_mode = BypassPermissions` every call is shadowed; otherwise each
/// `allowed_tools` entry that allows a whole tool is named (plus an implicit
/// `Skill` when `skills` is [`SkillsConfig::All`], since the transport appends
/// a bare `Skill` to the effective allowed tools).
pub fn can_use_tool_shadowed_warning(
    options: &super::ClaudeAgentOptions,
) -> Option<CanUseToolShadowedWarning> {
    options.can_use_tool.as_ref()?;
    let mut allowed_tools = options.allowed_tools.clone();
    if matches!(options.skills, Some(SkillsConfig::All))
        && !allowed_tools.iter().any(|t| t == "Skill")
    {
        allowed_tools.push("Skill".to_string());
    }
    shadowed_warning_message(options.permission_mode, &allowed_tools)
        .map(|message| CanUseToolShadowedWarning { message })
}

/// Emit the [`CanUseToolShadowedWarning`] via `tracing::warn!`, if any. Call
/// once per query construction / connect, after the `can_use_tool` /
/// `permission_prompt_tool_name` mutual-exclusion check.
pub(crate) fn warn_if_can_use_tool_shadowed(options: &super::ClaudeAgentOptions) {
    if let Some(warning) = can_use_tool_shadowed_warning(options) {
        tracing::warn!(
            target: "claude_agent_sdk::CanUseToolShadowedWarning",
            "{}",
            warning.message
        );
    }
}

#[cfg(test)]
mod tests {
    use super::super::PermissionUpdate;
    use super::*;

    #[test]
    fn whole_tool_allowed_mirrors_python() {
        assert_eq!(whole_tool_allowed("Read"), Some("Read"));
        assert_eq!(whole_tool_allowed("Read()"), Some("Read"));
        assert_eq!(whole_tool_allowed("Read(*)"), Some("Read"));
        assert_eq!(whole_tool_allowed("Bash(ls:*)"), None);
        assert_eq!(whole_tool_allowed("   "), None);
        assert_eq!(whole_tool_allowed(""), None);
        assert_eq!(whole_tool_allowed("(Read)"), None);
        assert_eq!(whole_tool_allowed("Read(x"), None);
    }

    #[test]
    fn permission_updates_to_wire_is_all_or_nothing() {
        let good = PermissionUpdate {
            r#type: "addDirectories".into(),
            rules: None,
            behavior: None,
            mode: None,
            directories: Some(vec!["/a".into()]),
            destination: None,
        };
        let bad = PermissionUpdate {
            r#type: "setMode".into(),
            ..good.clone()
        };
        assert_eq!(
            permission_updates_to_wire(std::slice::from_ref(&good)),
            Some(vec![serde_json::json!({
                "type": "addDirectories",
                "destination": "session",
                "directories": ["/a"]
            })])
        );
        assert_eq!(permission_updates_to_wire(&[good, bad]), None);
        assert_eq!(permission_updates_to_wire(&[]), Some(vec![]));
    }

    #[test]
    fn suggestions_parse_leniently() {
        let parsed = permission_suggestions_from_values(&[
            serde_json::json!({"type": "setMode", "mode": "futureMode"}),
            serde_json::json!({"type": "futureType", "extra": 1}),
        ])
        .unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].r#type, "setMode");
        assert!(parsed[0].mode.is_none());
        assert_eq!(parsed[1].r#type, "futureType");
        assert!(
            permission_suggestions_from_values(&[serde_json::json!({"mode": "plan"})]).is_err()
        );
    }
}
