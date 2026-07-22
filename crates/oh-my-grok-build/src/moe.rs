//! Mixture-of-Experts (MoE) provider routing for `omgb`.
//!
//! Picks the cheapest available provider (by approximate cost per 1M tokens) and
//! applies keyword tie-breakers when the task hints at local, fast, code, or
//! cheap preferences.

use std::cmp::Ordering;
use std::collections::HashMap;

use anyhow::{Result, bail};

use crate::providers::{
    ProviderConfig, add_discovered_providers, discover_local_models, env_var_name,
    is_local_provider_id, is_provider_reachable, is_valid_env_key, load_env_file, load_omg_config,
};

/// Approximate cost index per 1M tokens (input+output average) for known
/// providers. Values are derived from each provider's public pricing docs
/// (2024-2025). Unknown cloud providers default to `5.0`; local providers are
/// treated as `0.0`.
const COSTS: &[(&str, f64)] = &[
    // Major cloud APIs (pricing per 1M tokens, avg of input + output).
    ("openai", 6.25),     // gpt-4o: $2.50 in / $10.00 out
    ("anthropic", 9.0),   // claude-3-5-sonnet: $3.00 in / $15.00 out
    ("claude-code", 9.0), // same Anthropic endpoint as above
    ("openrouter", 9.5),  // pass-through + ~5% fee; default claude-3.5-sonnet
    ("xai", 4.0),         // grok-4.5: $2.00 in / $6.00 out
    ("codex", 3.75),      // codex-mini-latest: $1.50 in / $6.00 out
    ("gemini", 0.19),     // gemini-1.5-flash: $0.075 in / $0.30 out
    ("deepseek", 0.21),   // deepseek-chat: $0.14 in / $0.28 out
    ("groq", 0.69),       // llama-3.3-70b-versatile: $0.59 in / $0.79 out
    ("mistral", 4.0),     // mistral-large-latest: $2.00 in / $6.00 out
    ("cohere", 6.25),     // command-r-plus: $2.50 in / $10.00 out
    ("together", 1.04),   // Llama-3.3-70B-Instruct-Turbo: $1.04 / $1.04
    ("fireworks", 0.9),   // llama-v3p1-70b-instruct: $0.90 / $0.90
    ("perplexity", 1.0),  // sonar: $1.00 / $1.00
    ("ai21", 0.3),        // jamba-1.5-mini: $0.20 in / $0.40 out
    ("deepinfra", 0.61),  // DeepSeek-V3: $0.32 in / $0.89 out
    // Coding-assistant / harness-style providers. These are wrappers that
    // forward to other providers, so their cost is an approximate midpoint
    // of the cloud providers they can reach.
    ("opencode", 5.0),      // multi-provider wrapper; defaults to Anthropic/OpenAI
    ("hermes", 5.0),        // OpenRouter-recommended multi-provider wrapper
    ("pi", 5.0),            // oh-my-pi multi-provider wrapper
    ("omp", 5.0),           // oh-my-pi multi-provider wrapper
    ("github-models", 5.0), // Azure-hosted OpenAI models; free tier + paid
    ("nvidia", 1.5),        // NIM Llama endpoints (approximate)
    ("sambanova", 0.8),     // Llama endpoints (approximate)
    ("lepton", 2.0),        // approximate
    ("siliconflow", 0.5),   // approximate
    // Keyless local providers (cost is electricity, not API spend).
    ("ollama", 0.0),
    ("lmstudio", 0.0),
    ("vllm", 0.0),
    ("llama-cpp", 0.0),
    ("tabby", 0.0),
    ("jan", 0.0),
    ("localai", 0.0),
    ("llamafile", 0.0),
    ("text-generation-webui", 0.0),
    ("koboldcpp", 0.0),
    ("mistral-rs", 0.0),
    ("sglang", 0.0),
    ("tensorrt-llm", 0.0),
    ("mlc-llm", 0.0),
    ("xinference", 0.0),
    ("faraday", 0.0),
    ("aichat", 0.0),
    ("ava", 0.0),
    ("exllamav2", 0.0),
    ("ctranslate2", 0.0),
    ("ctransformers", 0.0),
    ("candle", 0.0),
    ("triton", 0.0),
    ("text-generation-inference", 0.0),
    ("lorax", 0.0),
];

const FAST_IDS: &[&str] = &[
    "groq",
    "fireworks",
    "together",
    "gemini",
    "openrouter",
    "deepseek",
    "perplexity",
    "mistral",
    "cohere",
];

const CODE_IDS: &[&str] = &[
    "codex",
    "claude-code",
    "opencode",
    "openai",
    "anthropic",
    "deepseek",
    "qwen",
    "mistral",
    "mixtral",
    "claude",
    "gemini",
    "groq",
    "fireworks",
    "together",
    "cohere",
];

pub fn provider_cost(id: &str) -> f64 {
    if let Some((_, cost)) = COSTS.iter().find(|(k, _)| *k == id) {
        return *cost;
    }
    if is_local_provider_id(id) {
        return 0.0;
    }
    5.0
}

fn is_fast_provider(id: &str) -> bool {
    FAST_IDS
        .iter()
        .any(|k| *k == id || id.starts_with(&format!("{k}-")))
}

fn is_code_provider(id: &str) -> bool {
    let lower = id.to_ascii_lowercase();
    CODE_IDS
        .iter()
        .any(|k| *k == lower || lower.starts_with(&format!("{k}-")))
}

/// Returns true when `word` appears in `task` as a whole word.
fn task_contains_word(task: &str, word: &str) -> bool {
    if task == word {
        return true;
    }
    let word = word.as_bytes();
    let task = task.as_bytes();
    for (i, window) in task.windows(word.len()).enumerate() {
        if window == word {
            let prev = if i > 0 { task[i - 1] } else { b' ' };
            let next = task.get(i + word.len()).copied().unwrap_or(b' ');
            if !prev.is_ascii_alphanumeric() && !next.is_ascii_alphanumeric() {
                return true;
            }
        }
    }
    false
}

/// Returns the configured providers that are available: they have a non-empty
/// `*_API_KEY` in `~/.omgb/.env` or the process environment, or (for loopback
/// local servers) respond to a `/models` probe. Also detects catalog templates
/// whose canonical env key is present even when they have not been explicitly
/// configured.
pub async fn available_providers() -> Result<Vec<String>> {
    let cfg = load_omg_config()?;
    let dotenv = load_env_file().unwrap_or_default();

    let mut ids = std::collections::HashSet::new();

    for provider in cfg.providers.values() {
        if provider_is_available(provider, &dotenv, true).await {
            ids.insert(provider.id.clone());
        }
    }

    for template in crate::providers::catalog::TEMPLATES {
        if let Some(provider) = crate::providers::provider_template(template.id)
            && provider_is_available(&provider, &dotenv, false).await
        {
            ids.insert(provider.id);
        }
    }

    let mut ids: Vec<String> = ids.into_iter().collect();
    ids.sort();
    ids.dedup();
    Ok(ids)
}

async fn provider_is_available(
    provider: &ProviderConfig,
    dotenv: &HashMap<String, String>,
    allow_loopback: bool,
) -> bool {
    if provider.model.trim().is_empty() {
        return false;
    }
    if allow_loopback && crate::net::is_url_host_loopback(&provider.base_url) {
        return is_provider_reachable(provider).await;
    }
    let storage = env_var_name(&provider.id);
    let mut keys: Vec<&str> = provider
        .env_key
        .as_deref()
        .unwrap_or(&[])
        .iter()
        .map(|s| s.as_str())
        .filter(|k| is_valid_env_key(k))
        .collect();
    if !keys.contains(&storage.as_str()) {
        keys.push(storage.as_str());
    }
    keys.into_iter().any(|k| {
        std::env::var(k).ok().filter(|v| !v.is_empty()).is_some()
            || dotenv.get(k).filter(|v| !v.is_empty()).is_some()
    })
}

/// Selects the cheapest available provider for `task` from an explicit list.
///
/// Keyword hints (`code`, `local`, `fast`, `cheap`) are used only as tie-breakers
/// after cost and key availability.
pub fn select_provider_from(available: &[String], task: &str) -> Result<String> {
    if available.is_empty() {
        bail!("no providers available (set *_API_KEY or use a loopback local server)");
    }

    let task_lower = task.to_ascii_lowercase();
    let mut scored: Vec<(&String, f64, i32)> = available
        .iter()
        .map(|id| {
            let cost = provider_cost(id);
            let mut tie = 0;
            if task_contains_word(&task_lower, "local") && is_local_provider_id(id) {
                tie += 3;
            }
            if task_contains_word(&task_lower, "fast") && is_fast_provider(id) {
                tie += 2;
            }
            if task_contains_word(&task_lower, "code") && is_code_provider(id) {
                tie += 1;
            }
            if task_contains_word(&task_lower, "cheap") && cost < 1.0 {
                tie += 1;
            }
            (id, cost, tie)
        })
        .collect();

    scored.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| b.2.cmp(&a.2))
            .then_with(|| a.0.cmp(b.0))
    });

    Ok(scored[0].0.clone())
}

/// Like [`select_provider`], but falls back to auto-discovering local model
/// servers (Ollama, LM Studio, vLLM, llama.cpp) when no keyed cloud provider is
/// available or when the task explicitly hints at local/cheap usage. Discovered
/// local providers are added to the config so `select_provider` can prefer them
/// over keyed cloud providers when the task hints at local/cheap usage.
pub async fn select_provider_or_fallback(prompt: &str) -> Result<String> {
    let task_lower = prompt.to_ascii_lowercase();
    let local_hint =
        task_contains_word(&task_lower, "local") || task_contains_word(&task_lower, "cheap");

    let mut available = available_providers().await?;
    if local_hint || available.is_empty() {
        let args = crate::args::DiscoverArgs {
            ollama_url: None,
            lmstudio_url: None,
            add: false,
        };
        let discovered = discover_local_models(&args).await?;
        if !discovered.is_empty() {
            add_discovered_providers(&discovered)?;
            available = available_providers().await?;
        }
    }

    if available.is_empty() {
        bail!("no providers available (set *_API_KEY or use a loopback local server)");
    }

    select_provider_from(&available, prompt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[allow(clippy::await_holding_lock)]
    async fn with_temp_home<F>(f: F)
    where
        F: for<'a> FnOnce(
            &'a std::path::Path,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + 'a>>,
    {
        let _guard = crate::OMGB_HOME_TEST_LOCK.lock().unwrap();
        let tmp = std::env::temp_dir().join(format!("omgb-moe-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&tmp).unwrap();
        unsafe { std::env::set_var("OMGB_HOME", &tmp) };

        let saved: Vec<(String, String)> = std::env::vars()
            .filter(|(k, _)| k.ends_with("_API_KEY"))
            .collect();
        for (k, _) in &saved {
            unsafe { std::env::remove_var(k) };
        }

        f(&tmp).await;

        for (k, v) in saved {
            unsafe { std::env::set_var(k, v) };
        }
        unsafe { std::env::remove_var("OMGB_HOME") };
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_provider_cost_known_cloud() {
        assert!((provider_cost("openai") - 6.25).abs() < 1e-9);
        assert!((provider_cost("anthropic") - 9.0).abs() < 1e-9);
        assert!((provider_cost("groq") - 0.69).abs() < 1e-9);
    }

    #[test]
    fn test_provider_cost_local_is_free() {
        assert!((provider_cost("ollama") - 0.0).abs() < 1e-9);
        assert!((provider_cost("jan") - 0.0).abs() < 1e-9);
    }

    #[test]
    fn test_provider_cost_unknown_defaults_to_five() {
        assert!((provider_cost("some-unknown-cloud") - 5.0).abs() < 1e-9);
    }

    #[test]
    fn test_provider_cost_cloud_harness_not_local() {
        assert!((provider_cost("codex") - 3.75).abs() < 1e-9);
        assert!((provider_cost("claude-code") - 9.0).abs() < 1e-9);
        assert!((provider_cost("hermes") - 5.0).abs() < 1e-9);
        assert!((provider_cost("opencode") - 5.0).abs() < 1e-9);
        assert!((provider_cost("pi") - 5.0).abs() < 1e-9);
    }

    #[test]
    fn test_select_provider_prefers_cheapest() {
        let available = vec!["openai".into(), "deepseek".into(), "ollama".into()];
        assert_eq!(select_provider_from(&available, "do it").unwrap(), "ollama");
    }

    #[test]
    fn test_select_provider_fast_tie_breaker() {
        let available = vec!["perplexity".into(), "fireworks".into()];
        // fireworks ($0.90) is cheaper than perplexity ($1.00) and both are fast.
        assert_eq!(
            select_provider_from(&available, "fast").unwrap(),
            "fireworks"
        );
    }

    #[test]
    fn test_select_provider_local_tie_breaker() {
        let available = vec!["ollama".into(), "lmstudio".into()];
        // Both cost 0.0 and are local; "local" keeps them tied, sorted by id.
        assert_eq!(
            select_provider_from(&available, "local").unwrap(),
            "lmstudio"
        );
    }

    #[test]
    fn test_select_provider_cheap_does_not_override_cost() {
        let available = vec!["openai".into(), "ollama".into()];
        assert_eq!(
            select_provider_from(&available, "cheap fast").unwrap(),
            "ollama"
        );
    }

    #[test]
    fn test_select_provider_empty_errors() {
        assert!(select_provider_from(&[], "task").is_err());
    }

    #[tokio::test]
    async fn test_available_providers_reads_env_and_config() {
        with_temp_home(|home| {
            Box::pin(async move {
                let provider = crate::providers::ProviderConfig {
                    id: "testprovider".into(),
                    name: "Test".into(),
                    model: "m".into(),
                    base_url: "https://example.com/v1".into(),
                    api_backend: None,
                    env_key: Some(vec!["OMGB_TESTPROVIDER_API_KEY".into()]),
                    extra_headers: None,
                    context_window: None,
                    auto_compact_threshold_percent: None,
                    temperature: None,
                    top_p: None,
                    max_completion_tokens: None,
                };
                let cfg = crate::providers::OmgConfig {
                    default_model: None,
                    providers: [("testprovider".into(), provider)]
                        .into_iter()
                        .collect::<HashMap<_, _>>(),
                    relay: None,
                };
                crate::providers::save_omg_config(&cfg).unwrap();
                std::fs::write(home.join(".env"), "OMGB_TESTPROVIDER_API_KEY=secret\n").unwrap();

                assert_eq!(available_providers().await.unwrap(), vec!["testprovider"]);
            })
        })
        .await;
    }

    #[tokio::test]
    async fn test_available_providers_uses_default_env_key() {
        with_temp_home(|home| {
            Box::pin(async move {
                let provider = crate::providers::ProviderConfig {
                    id: "myprov".into(),
                    name: "My".into(),
                    model: "m".into(),
                    base_url: "https://example.com/v1".into(),
                    api_backend: None,
                    env_key: None,
                    extra_headers: None,
                    context_window: None,
                    auto_compact_threshold_percent: None,
                    temperature: None,
                    top_p: None,
                    max_completion_tokens: None,
                };
                let cfg = crate::providers::OmgConfig {
                    default_model: None,
                    providers: [("myprov".into(), provider)]
                        .into_iter()
                        .collect::<HashMap<_, _>>(),
                    relay: None,
                };
                crate::providers::save_omg_config(&cfg).unwrap();
                std::fs::write(home.join(".env"), "OMGB_MYPROV_API_KEY=secret\n").unwrap();

                assert_eq!(available_providers().await.unwrap(), vec!["myprov"]);
            })
        })
        .await;
    }

    #[tokio::test]
    async fn test_available_providers_detects_catalog_keys() {
        with_temp_home(|home| {
            Box::pin(async move {
                std::fs::write(home.join(".env"), "OPENAI_API_KEY=sk-secret\n").unwrap();
                let providers = available_providers().await.unwrap();
                assert!(providers.contains(&"openai".to_string()));
            })
        })
        .await;
    }

    #[tokio::test]
    async fn test_available_providers_skips_empty_model() {
        with_temp_home(|home| {
            Box::pin(async move {
                let provider = crate::providers::ProviderConfig {
                    id: "emptymodel".into(),
                    name: "Empty".into(),
                    model: String::new(),
                    base_url: "https://example.com/v1".into(),
                    api_backend: None,
                    env_key: Some(vec!["OMGB_EMPTYMODEL_API_KEY".into()]),
                    extra_headers: None,
                    context_window: None,
                    auto_compact_threshold_percent: None,
                    temperature: None,
                    top_p: None,
                    max_completion_tokens: None,
                };
                let cfg = crate::providers::OmgConfig {
                    default_model: None,
                    providers: [("emptymodel".into(), provider)]
                        .into_iter()
                        .collect::<HashMap<_, _>>(),
                    relay: None,
                };
                crate::providers::save_omg_config(&cfg).unwrap();
                std::fs::write(home.join(".env"), "OMGB_EMPTYMODEL_API_KEY=secret\n").unwrap();

                let providers = available_providers().await.unwrap();
                assert!(
                    !providers.contains(&"emptymodel".to_string()),
                    "provider with empty model should not be auto-routed"
                );
            })
        })
        .await;
    }
}
