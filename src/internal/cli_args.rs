use std::path::Path;

use crate::error::{CLIConnectionError, ClaudeSDKError, Result};
use crate::types::{SettingSource, SkillsConfig};

use super::transport::TransportOptions;

/// cmd.exe metacharacters, plus the quote character cmd.exe uses to toggle its
/// quoting state and "!", which expands like "%" under delayed expansion.
const CMD_EXE_METACHARACTERS: &str = "&|<>^%!\"";

fn serialize_cli_value<T: serde::Serialize>(value: &T) -> Result<String> {
    let value = serde_json::to_value(value)?;
    match value {
        serde_json::Value::String(s) => Ok(s),
        other => Ok(other.to_string()),
    }
}

fn serialize_setting_sources(sources: &[SettingSource]) -> Result<String> {
    sources
        .iter()
        .map(serialize_cli_value)
        .collect::<Result<Vec<_>>>()
        .map(|sources| sources.join(","))
}

/// Python `repr()` of a `str`, for error messages that quote values verbatim.
pub(crate) fn py_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::with_capacity(value.len() + 2);
    out.push(quote);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || (0x7f..=0xa0).contains(&(c as u32)) => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `str.isspace` (Rust's White_Space set plus the C0 separators).
fn py_isspace(c: char) -> bool {
    c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c)
}

fn is_invalid_skill_char(c: char) -> bool {
    matches!(c, '(' | ')' | ',' | '\u{feff}')
        || (c as u32) <= 0x1f
        || (0x7f..=0x9f).contains(&(c as u32))
}

/// Reject skill names that cannot ride safely in a `Skill(name)` rule
/// (Python `_validate_skill_name`).
pub(crate) fn validate_skill_name(name: &str) -> Result<()> {
    let err = |message: String| Err(ClaudeSDKError::Other(message));
    let stripped = name.trim_matches(py_isspace);
    let repr = py_repr(name);
    if stripped.is_empty() {
        return err("Skill names must be non-empty strings".to_string());
    }
    if name != stripped {
        return err(format!(
            "Invalid skill name {repr}: leading or trailing whitespace can never match \u{2014} the Skill tool trims the invoked name."
        ));
    }
    if name.chars().any(is_invalid_skill_char) {
        return err(format!(
            "Invalid skill name {repr}: parentheses, commas, control characters, and byte-order marks are not allowed. Names match the skill's directory name, or 'plugin:skill' for plugin-qualified skills."
        ));
    }
    if name == "*" {
        return err(
            "Invalid skill name '*': use skills=\"all\" to enable every skill.".to_string(),
        );
    }
    if name.ends_with(":*") || name.ends_with(" *") {
        return err(format!(
            "Invalid skill name {repr}: wildcard-suffix names are not allowed; list each skill by its exact name."
        ));
    }
    if name.starts_with('/') {
        return err(format!(
            "Invalid skill name {repr}: skill names may not start with '/'. The skills option takes the canonical name, not the slash-command form."
        ));
    }
    if name.contains("\\\\") {
        return err(format!(
            "Invalid skill name {repr}: consecutive backslashes are not allowed \u{2014} the per-rule parser collapses them, so the rule would name a different skill."
        ));
    }
    if name.ends_with('\\') {
        return err(format!(
            "Invalid skill name {repr}: names may not end with an unpaired backslash."
        ));
    }
    Ok(())
}

/// Windows-only defense in depth: reject cmd.exe metacharacters in values
/// that commonly come from external input. `windows` is a parameter so the
/// logic is testable everywhere; callers pass `cfg!(windows)`.
pub(crate) fn reject_windows_cmd_metacharacters(
    option_name: &str,
    value: &str,
    windows: bool,
) -> Result<()> {
    if !windows {
        return Ok(());
    }
    let mut bad: Vec<char> = value
        .chars()
        .filter(|c| CMD_EXE_METACHARACTERS.contains(*c) || *c == '\r' || *c == '\n')
        .collect();
    bad.sort_unstable();
    bad.dedup();
    if bad.is_empty() {
        return Ok(());
    }
    let bad = bad
        .iter()
        .map(|c| py_repr(&c.to_string()))
        .collect::<Vec<_>>()
        .join(", ");
    Err(ClaudeSDKError::Other(format!(
        "{option_name} value {} contains characters that are unsafe to pass on a Windows command line: [{bad}]",
        py_repr(value)
    )))
}

fn effective_allowed_tools(options: &TransportOptions) -> Result<Vec<String>> {
    let mut allowed_tools = options.allowed_tools.clone();

    match &options.skills {
        Some(SkillsConfig::All) if !allowed_tools.iter().any(|tool| tool == "Skill") => {
            allowed_tools.push("Skill".to_string());
        }
        Some(SkillsConfig::Names(names)) => {
            for name in names {
                validate_skill_name(name)?;
                let pattern = format!("Skill({name})");
                if !allowed_tools.iter().any(|tool| tool == &pattern) {
                    allowed_tools.push(pattern);
                }
            }
        }
        Some(SkillsConfig::All) | None => {}
    }

    Ok(allowed_tools)
}

fn effective_setting_sources(options: &TransportOptions) -> Option<Vec<SettingSource>> {
    if let Some(sources) = &options.setting_sources {
        return Some(sources.clone());
    }

    options
        .skills
        .as_ref()
        .map(|_| vec![SettingSource::User, SettingSource::Project])
}

/// Load the caller's settings for the sandbox merge. Read a path as the CLI
/// does: relative to `cwd`, with or without a BOM.
fn load_settings_object(
    settings: Option<&str>,
    cwd: Option<&str>,
) -> std::result::Result<serde_json::Map<String, serde_json::Value>, String> {
    let Some(settings) = settings else {
        return Ok(serde_json::Map::new());
    };
    let trimmed = settings.trim();
    let text = if trimmed.starts_with('{') && trimmed.ends_with('}') {
        trimmed.to_string()
    } else {
        let path = Path::new(cwd.unwrap_or("")).join(trimmed);
        let content = std::fs::read_to_string(&path)
            .map_err(|e| format!("{e}: {}", py_repr(&path.to_string_lossy())))?;
        content
            .strip_prefix('\u{feff}')
            .map(str::to_string)
            .unwrap_or(content)
    };
    match serde_json::from_str::<serde_json::Value>(&text).map_err(|e| e.to_string())? {
        serde_json::Value::Object(map) => Ok(map),
        other => Err(format!(
            "settings must be a JSON object, got {}",
            match other {
                serde_json::Value::Array(_) => "an array",
                serde_json::Value::String(_) => "a string",
                serde_json::Value::Number(_) => "a number",
                serde_json::Value::Bool(_) => "a boolean",
                _ => "null",
            }
        )),
    }
}

/// Build the `--settings` value, merging sandbox settings in if provided.
///
/// Settings that can't be loaded fail closed: launching without them would
/// drop the caller's permission rules and hooks.
fn build_settings_value(options: &TransportOptions) -> Result<Option<String>> {
    let Some(sandbox) = &options.sandbox else {
        return Ok(options.settings.clone());
    };

    let mut settings = load_settings_object(options.settings.as_deref(), options.cwd.as_deref())
        .map_err(|e| CLIConnectionError::new(format!("Failed to load settings: {e}")))?;
    settings.insert("sandbox".to_string(), serde_json::to_value(sandbox)?);

    Ok(Some(serde_json::Value::Object(settings).to_string()))
}

/// Python `str(float)`: integral values keep a trailing `.0`.
fn python_float_str(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e16 {
        format!("{value:.1}")
    } else {
        value.to_string()
    }
}

fn push_flag(args: &mut Vec<String>, flag: &str, value: impl Into<String>) {
    args.push(flag.to_string());
    args.push(value.into());
}

/// Build the CLI argv (without the executable), in the same order as the
/// Python SDK's `_build_command`.
pub(crate) fn build_cli_args(options: &TransportOptions) -> Result<Vec<String>> {
    let windows = cfg!(windows);
    let mut args = vec![
        "--output-format".to_string(),
        "stream-json".to_string(),
        "--verbose".to_string(),
    ];

    if let Some(ref file) = options.system_prompt_file {
        push_flag(&mut args, "--system-prompt-file", file.path.clone());
    } else if let Some(ref custom) = options.system_prompt_custom {
        push_flag(&mut args, "--system-prompt", custom.prompt.clone());
    } else if let Some(ref preset) = options.system_prompt_preset {
        if let Some(ref append) = preset.append {
            push_flag(&mut args, "--append-system-prompt", append.clone());
        }
    } else {
        let prompt = options.system_prompt.as_deref().unwrap_or("");
        push_flag(&mut args, "--system-prompt", prompt);
    }

    if options.tools_preset.is_some() {
        // Any preset object maps to the CLI's `default` tool set.
        push_flag(&mut args, "--tools", "default");
    } else if options.tools_set {
        push_flag(&mut args, "--tools", options.tools.join(","));
    }

    let allowed_tools = effective_allowed_tools(options)?;
    if !allowed_tools.is_empty() {
        push_flag(&mut args, "--allowedTools", allowed_tools.join(","));
    }

    if let Some(turns) = options.max_turns.filter(|turns| *turns != 0) {
        push_flag(&mut args, "--max-turns", turns.to_string());
    }

    if let Some(budget) = options.max_budget_usd {
        push_flag(&mut args, "--max-budget-usd", python_float_str(budget));
    }

    if !options.disallowed_tools.is_empty() {
        push_flag(
            &mut args,
            "--disallowedTools",
            options.disallowed_tools.join(","),
        );
    }

    if let Some(ref task_budget) = options.task_budget {
        push_flag(&mut args, "--task-budget", task_budget.total.to_string());
    }

    if let Some(model) = options.model.as_deref().filter(|v| !v.is_empty()) {
        push_flag(&mut args, "--model", model);
    }

    if let Some(fallback) = options.fallback_model.as_deref().filter(|v| !v.is_empty()) {
        push_flag(&mut args, "--fallback-model", fallback);
    }

    if !options.betas.is_empty() {
        let betas: Vec<String> = options
            .betas
            .iter()
            .map(serialize_cli_value)
            .collect::<Result<Vec<_>>>()?;
        push_flag(&mut args, "--betas", betas.join(","));
    }

    if let Some(name) = options
        .permission_prompt_tool_name
        .as_deref()
        .filter(|v| !v.is_empty())
    {
        push_flag(&mut args, "--permission-prompt-tool", name);
    }

    if let Some(mode) = options.permission_mode {
        push_flag(&mut args, "--permission-mode", serialize_cli_value(&mode)?);
    }

    if options.continue_conversation {
        args.push("--continue".to_string());
    }

    // `--flag=value`, not two argv tokens: the CLI declares --resume with an
    // optional value, so in the two-token form a dash-leading value would be
    // parsed as a separate flag (flag injection).
    if let Some(resume) = options.resume.as_deref().filter(|v| !v.is_empty()) {
        reject_windows_cmd_metacharacters("resume", resume, windows)?;
        args.push(format!("--resume={resume}"));
    }

    if let Some(session_id) = options.session_id.as_deref().filter(|v| !v.is_empty()) {
        reject_windows_cmd_metacharacters("session_id", session_id, windows)?;
        args.push(format!("--session-id={session_id}"));
    }

    if let Some(settings) = build_settings_value(options)?.filter(|v| !v.is_empty()) {
        push_flag(&mut args, "--settings", settings);
    }

    for dir in &options.add_dirs {
        push_flag(&mut args, "--add-dir", dir.clone());
    }

    if let Some(config) = &options.mcp_servers_config {
        if !config.is_empty() {
            push_flag(&mut args, "--mcp-config", config.clone());
        }
    } else if !options.mcp_servers.is_empty() {
        let mcp_config = serde_json::json!({
            "mcpServers": options.mcp_servers
        });
        push_flag(&mut args, "--mcp-config", mcp_config.to_string());
    }

    if options.include_partial_messages {
        args.push("--include-partial-messages".to_string());
    }

    if options.include_hook_events {
        args.push("--include-hook-events".to_string());
    }

    if options.strict_mcp_config {
        args.push("--strict-mcp-config".to_string());
    }

    if options.fork_session {
        args.push("--fork-session".to_string());
    }

    if let Some(at) = options
        .resume_session_at
        .as_deref()
        .filter(|v| !v.is_empty())
    {
        reject_windows_cmd_metacharacters("resume_session_at", at, windows)?;
        args.push(format!("--resume-session-at={at}"));
    }

    // `is_some`, not truthiness: an empty string is forwarded so the CLI
    // rejects it instead of the SDK silently disarming the guard.
    if let Some(drops) = options.resume_drops_turn.as_deref() {
        reject_windows_cmd_metacharacters("resume_drops_turn", drops, windows)?;
        args.push(format!("--resume-drops-turn={drops}"));
    }

    if options.session_store_enabled {
        args.push("--session-mirror".to_string());
    }

    if let Some(setting_sources) = effective_setting_sources(options) {
        let setting_sources = serialize_setting_sources(&setting_sources)?;
        args.push(format!("--setting-sources={setting_sources}"));
    }

    for plugin in &options.plugins {
        if plugin.r#type != "local" {
            return Err(ClaudeSDKError::Other(format!(
                "Unsupported plugin type: {}",
                plugin.r#type
            )));
        }
        push_flag(&mut args, "--plugin-dir", plugin.path.clone());
    }

    for (flag, value) in &options.extra_args {
        match value {
            None => args.push(format!("--{flag}")),
            // A dash-leading value would parse as a separate flag in the
            // two-token form; the equals form always binds it.
            Some(value) if value.starts_with('-') => args.push(format!("--{flag}={value}")),
            Some(value) => push_flag(&mut args, &format!("--{flag}"), value.clone()),
        }
    }

    if let Some(ref thinking) = options.thinking {
        match thinking.r#type {
            crate::types::ThinkingConfigType::Adaptive => {
                push_flag(&mut args, "--thinking", "adaptive");
            }
            crate::types::ThinkingConfigType::Enabled => {
                if let Some(tokens) = thinking.budget_tokens {
                    push_flag(&mut args, "--max-thinking-tokens", tokens.to_string());
                }
            }
            crate::types::ThinkingConfigType::Disabled => {
                push_flag(&mut args, "--thinking", "disabled");
            }
        }
        if thinking.r#type != crate::types::ThinkingConfigType::Disabled {
            if let Some(ref display) = thinking.display {
                push_flag(
                    &mut args,
                    "--thinking-display",
                    serialize_cli_value(display)?,
                );
            }
        }
    } else if let Some(tokens) = options.max_thinking_tokens {
        push_flag(&mut args, "--max-thinking-tokens", tokens.to_string());
    }

    if let Some(effort) = options.effort {
        push_flag(&mut args, "--effort", effort.as_cli());
    }

    if let Some(ref output_format) = options.output_format {
        if output_format.get("type").and_then(|v| v.as_str()) == Some("json_schema") {
            if let Some(schema) = output_format.get("schema") {
                push_flag(&mut args, "--json-schema", schema.to_string());
            }
        }
    }

    // Always last, matching the Python SDK.
    push_flag(&mut args, "--input-format", "stream-json");

    Ok(args)
}
