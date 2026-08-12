use anyhow::Result;
use std::collections::BTreeMap;

use crate::args::{AuthArgs, AuthCommand};

pub async fn run_auth(args: AuthArgs) -> Result<()> {
    match args.command {
        AuthCommand::Status => status().await,
        AuthCommand::Login { browser } => login(browser).await,
        AuthCommand::Logout => logout(),
    }
}

async fn status() -> Result<()> {
    let providers = crate::providers::load_omg_config()?;
    let config = crate::build_agent_config(None)?;
    let mut models = BTreeMap::new();
    for (id, provider) in &providers.providers {
        models.insert(format!("omgb-{id}"), provider.model.clone());
    }
    for (id, model) in &config.config_models {
        if id.starts_with("omgb-") {
            models
                .entry(id.clone())
                .or_insert_with(|| model.model.clone().unwrap_or_else(|| id.clone()));
        }
    }
    let configured_default = crate::providers::configured_default_model()?;
    let default_model = configured_default
        .as_deref()
        .or(config.models.default.as_deref())
        .or(providers.default_model.as_deref());
    if models.is_empty() {
        println!("BYOK/local models: none configured");
    } else {
        println!("BYOK/local models:");
        for (id, model) in &models {
            println!(
                "  {id}: {}{}",
                model,
                if default_model == Some(id.as_str()) {
                    " (default)"
                } else {
                    ""
                }
            );
        }
    }
    if let Some(default) = default_model.filter(|model| !models.contains_key(*model)) {
        println!("Default model: {default} (Grok subscription)");
    }

    match xai_grok_shell::auth::try_ensure_fresh_auth(&config.grok_com_config).await {
        Some(auth) => match auth.auth_mode {
            xai_grok_shell::auth::AuthMode::ApiKey => {
                println!("xAI BYOK API key: configured")
            }
            xai_grok_shell::auth::AuthMode::External => {
                println!("External authentication: active")
            }
            xai_grok_shell::auth::AuthMode::Oidc | xai_grok_shell::auth::AuthMode::WebLogin => {
                match auth.email {
                    Some(email) => println!("Grok subscription: signed in as {email}"),
                    None => println!("Grok subscription: signed in"),
                }
            }
        },
        None => println!("Grok subscription: not signed in (optional)"),
    }

    if models.is_empty() {
        println!(
            "\nChoose BYOK/local first with `omgb provider catalog` or `omgb provider discover --add`."
        );
        println!("To use a Grok subscription instead, run `omgb auth login`.");
    }
    Ok(())
}

async fn login(browser: bool) -> Result<()> {
    let config = crate::build_agent_config(None)?;
    xai_grok_shell::auth::run_cli_login(&config, browser, !browser, false).await
}

fn logout() -> Result<()> {
    let config = crate::build_agent_config(None)?;
    xai_grok_shell::auth::run_cli_logout(&config)
}
