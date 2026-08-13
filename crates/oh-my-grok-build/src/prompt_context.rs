//! Cache-stable compilation of supplemental harness context.

use sha2::{Digest, Sha256};

const CACHE_AFFINITY_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledPromptContext {
    pub rules: Option<String>,
    pub stable_prefix_sha256: Option<String>,
    pub stable_bytes: usize,
    pub volatile_bytes: usize,
}

/// Stable execution-policy inputs that affect whether two model requests are
/// safe to treat as members of the same prompt-cache affinity group.
///
/// These values are hashed locally and are never written to telemetry. This is
/// intentionally separate from prompt text: a permission/tool/sandbox change
/// must invalidate affinity even when the supplemental prompt prefix did not
/// change.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct WorkspacePolicy {
    pub sandbox_profile: Option<String>,
    pub yolo: bool,
    pub trust: bool,
    pub permission_mode: Option<String>,
    pub cli_tools: Option<String>,
    pub cli_disallowed_tools: Option<String>,
    pub allow_rules: Vec<String>,
    pub deny_rules: Vec<String>,
    pub disable_web_search: bool,
    pub agent: Option<String>,
    pub agent_manifest_sha256: Option<String>,
    pub reasoning_effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CacheAffinity {
    pub cache_affinity_sha256: String,
    pub stable_prefix_sha256: String,
    pub workspace_policy_sha256: String,
}

fn sha256(value: &[u8]) -> String {
    format!("{:x}", Sha256::digest(value))
}

pub fn content_sha256(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| !value.is_empty())
        .and_then(|value| normalize_block(value.to_string()))
        .map(|value| sha256(value.as_bytes()))
}

fn canonical_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(canonical_json).collect())
        }
        serde_json::Value::Object(values) => {
            let mut entries = values.into_iter().collect::<Vec<_>>();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            serde_json::Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key, canonical_json(value)))
                    .collect(),
            )
        }
        value => value,
    }
}

pub fn json_content_sha256(value: Option<&str>) -> Option<String> {
    let value = value.filter(|value| !value.is_empty())?;
    match serde_json::from_str::<serde_json::Value>(value) {
        Ok(parsed) => serde_json::to_vec(&canonical_json(parsed))
            .ok()
            .map(|canonical| sha256(&canonical)),
        Err(_) => content_sha256(Some(value)),
    }
}

fn normalize_set(values: &mut Vec<String>) {
    values.retain(|value| !value.trim().is_empty());
    for value in values.iter_mut() {
        *value = value.trim().replace("\r\n", "\n").replace('\r', "\n");
    }
    values.sort();
    values.dedup();
}

fn normalized_policy(mut policy: WorkspacePolicy) -> WorkspacePolicy {
    fn normalize_option(value: &mut Option<String>) {
        *value = value.take().and_then(normalize_block);
    }

    fn normalize_csv(value: &mut Option<String>) {
        let mut items = value
            .take()
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>();
        items.sort();
        items.dedup();
        *value = (!items.is_empty()).then(|| items.join(","));
    }

    normalize_option(&mut policy.sandbox_profile);
    normalize_option(&mut policy.permission_mode);
    normalize_csv(&mut policy.cli_tools);
    normalize_csv(&mut policy.cli_disallowed_tools);
    normalize_set(&mut policy.allow_rules);
    normalize_set(&mut policy.deny_rules);
    normalize_option(&mut policy.agent);
    normalize_option(&mut policy.agent_manifest_sha256);
    normalize_option(&mut policy.reasoning_effort);
    policy
}

/// Derive a privacy-safe, deterministic cache affinity. Only the returned
/// hashes may be emitted; the provider/model/policy inputs remain local.
pub fn cache_affinity(
    context: &CompiledPromptContext,
    provider: &str,
    model: &str,
    provider_execution_fingerprint: Option<&str>,
    policy: WorkspacePolicy,
) -> Option<CacheAffinity> {
    let stable_prefix_sha256 = context.stable_prefix_sha256.clone()?;
    let policy = normalized_policy(policy);
    let policy_bytes = serde_json::to_vec(&policy).ok()?;
    let workspace_policy_sha256 = sha256(&policy_bytes);
    let affinity_bytes = serde_json::to_vec(&serde_json::json!({
        "version": CACHE_AFFINITY_VERSION,
        "provider": provider.trim().to_ascii_lowercase(),
        "model": model.trim(),
        "provider_execution_fingerprint": provider_execution_fingerprint,
        "stable_prefix_sha256": stable_prefix_sha256,
        "workspace_policy_sha256": workspace_policy_sha256,
    }))
    .ok()?;
    Some(CacheAffinity {
        cache_affinity_sha256: sha256(&affinity_bytes),
        stable_prefix_sha256,
        workspace_policy_sha256,
    })
}

fn normalize_block(value: String) -> Option<String> {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    let normalized = normalized
        .lines()
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n");
    let normalized = normalized.trim().to_string();
    (!normalized.is_empty()).then_some(normalized)
}

fn compile_lane(mut parts: Vec<(&'static str, String)>) -> String {
    parts.sort_by_key(|(key, _)| *key);
    parts
        .into_iter()
        .filter_map(|(_, value)| normalize_block(value))
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Compile stable harness policy before per-turn memory and other volatile data.
///
/// Stable components are sorted by their fixed component key so call-site order,
/// map iteration, or platform newlines cannot churn the provider's reusable
/// prompt prefix. Volatile components are deliberately placed last.
pub fn compile(
    stable: Vec<(&'static str, String)>,
    volatile: Vec<(&'static str, String)>,
) -> CompiledPromptContext {
    let stable = compile_lane(stable);
    let volatile = compile_lane(volatile);
    let stable_prefix_sha256 =
        (!stable.is_empty()).then(|| format!("{:x}", Sha256::digest(stable.as_bytes())));
    let stable_bytes = stable.len();
    let volatile_bytes = volatile.len();
    let rules = match (stable.is_empty(), volatile.is_empty()) {
        (true, true) => None,
        (false, true) => Some(stable),
        (true, false) => Some(volatile),
        (false, false) => Some(format!("{stable}\n\n{volatile}")),
    };
    CompiledPromptContext {
        rules,
        stable_prefix_sha256,
        stable_bytes,
        volatile_bytes,
    }
}

pub fn record_cache_shape(context: &CompiledPromptContext) {
    let Some(hash) = context.stable_prefix_sha256.as_deref() else {
        return;
    };
    let _ = crate::timeline::add_event(
        "prompt_cache",
        "compiled cache-stable supplemental context",
        Some(serde_json::json!({
            "stable_prefix_sha256": hash,
            "stable_bytes": context.stable_bytes,
            "volatile_bytes": context.volatile_bytes,
        })),
    );
}

pub fn record_cache_affinity(
    context: &CompiledPromptContext,
    affinity: &CacheAffinity,
    attempt: usize,
) {
    let _ = crate::timeline::add_event(
        "prompt_cache",
        "prepared privacy-safe prompt cache affinity",
        Some(serde_json::json!({
            "cache_affinity_sha256": affinity.cache_affinity_sha256,
            "stable_prefix_sha256": affinity.stable_prefix_sha256,
            "workspace_policy_sha256": affinity.workspace_policy_sha256,
            "stable_bytes": context.stable_bytes,
            "volatile_bytes": context.volatile_bytes,
            "attempt": attempt,
        })),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_hash_ignores_component_order_and_platform_newlines() {
        let first = compile(
            vec![
                ("20-taste", "taste\r\n".into()),
                ("10-skills", "skill".into()),
            ],
            vec![],
        );
        let second = compile(
            vec![
                ("10-skills", "skill\n".into()),
                ("20-taste", "taste".into()),
            ],
            vec![],
        );
        assert_eq!(first, second);
        assert_eq!(first.rules.as_deref(), Some("skill\n\ntaste"));
    }

    #[test]
    fn volatile_changes_do_not_change_the_cache_prefix_hash() {
        let first = compile(
            vec![("10-skills", "stable".into())],
            vec![("90-memory", "turn one".into())],
        );
        let second = compile(
            vec![("10-skills", "stable".into())],
            vec![("90-memory", "turn two".into())],
        );
        assert_eq!(first.stable_prefix_sha256, second.stable_prefix_sha256);
        assert_ne!(first.rules, second.rules);
        assert!(first.rules.unwrap().starts_with("stable\n\nturn one"));
    }

    #[test]
    fn affinity_is_stable_for_reordered_policy_sets() {
        let context = compile(vec![("10-skills", "stable".into())], vec![]);
        let first = cache_affinity(
            &context,
            "OpenAI",
            "gpt-5.6",
            Some("provider-fingerprint"),
            WorkspacePolicy {
                allow_rules: vec!["tool:b".into(), "tool:a".into()],
                deny_rules: vec!["shell".into(), "web".into()],
                ..WorkspacePolicy::default()
            },
        )
        .unwrap();
        let second = cache_affinity(
            &context,
            "openai",
            "gpt-5.6",
            Some("provider-fingerprint"),
            WorkspacePolicy {
                allow_rules: vec!["tool:a".into(), "tool:b".into(), "tool:a".into()],
                deny_rules: vec!["web".into(), "shell".into()],
                ..WorkspacePolicy::default()
            },
        )
        .unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn affinity_changes_when_policy_or_provider_execution_changes() {
        let context = compile(vec![("10-skills", "stable".into())], vec![]);
        let baseline = cache_affinity(
            &context,
            "custom",
            "model",
            Some("runtime-a"),
            WorkspacePolicy::default(),
        )
        .unwrap();
        let yolo = cache_affinity(
            &context,
            "custom",
            "model",
            Some("runtime-a"),
            WorkspacePolicy {
                yolo: true,
                ..WorkspacePolicy::default()
            },
        )
        .unwrap();
        let moved = cache_affinity(
            &context,
            "custom",
            "model",
            Some("runtime-b"),
            WorkspacePolicy::default(),
        )
        .unwrap();
        assert_ne!(baseline, yolo);
        assert_ne!(baseline, moved);
    }

    #[test]
    fn volatile_content_never_enters_affinity() {
        let first = compile(
            vec![("10-skills", "stable".into())],
            vec![("90-memory", "secret turn one".into())],
        );
        let second = compile(
            vec![("10-skills", "stable".into())],
            vec![("90-memory", "secret turn two".into())],
        );
        let first = cache_affinity(
            &first,
            "provider",
            "model",
            None,
            WorkspacePolicy::default(),
        )
        .unwrap();
        let second = cache_affinity(
            &second,
            "provider",
            "model",
            None,
            WorkspacePolicy::default(),
        )
        .unwrap();
        assert_eq!(first, second);
    }

    #[test]
    fn logical_json_and_tool_order_share_affinity() {
        let context = compile(vec![("10-skills", "stable".into())], vec![]);
        let first_manifest = json_content_sha256(Some(r#"{"b":2,"a":{"x":1}}"#));
        let second_manifest = json_content_sha256(Some("{\n  \"a\": { \"x\": 1 },\n  \"b\": 2\n}"));
        assert_eq!(first_manifest, second_manifest);
        let first = cache_affinity(
            &context,
            "provider",
            "model",
            None,
            WorkspacePolicy {
                cli_tools: Some("browser,shell,browser".into()),
                agent_manifest_sha256: first_manifest,
                ..WorkspacePolicy::default()
            },
        )
        .unwrap();
        let second = cache_affinity(
            &context,
            "provider",
            "model",
            None,
            WorkspacePolicy {
                cli_tools: Some(" shell, browser ".into()),
                agent_manifest_sha256: second_manifest,
                ..WorkspacePolicy::default()
            },
        )
        .unwrap();
        assert_eq!(first, second);
    }
}
