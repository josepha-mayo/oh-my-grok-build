//! Cache-stable compilation of supplemental harness context.

use sha2::{Digest, Sha256};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledPromptContext {
    pub rules: Option<String>,
    pub stable_prefix_sha256: Option<String>,
    pub stable_bytes: usize,
    pub volatile_bytes: usize,
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
}
