use anyhow::Result;
use fabro_api::types;
use fabro_auth::credential_id_for;
use fabro_config::legacy_env;
use fabro_types::settings::CliSettings;
use fabro_types::settings::cli::CliLayer;
use fabro_util::printer::Printer;
use fabro_util::terminal::Styles;

use crate::args::{ProviderLoginArgs, require_no_json_override};
use crate::command_context::CommandContext;
use crate::shared::provider_auth;

pub(super) async fn login_command(
    args: ProviderLoginArgs,
    cli: &CliSettings,
    cli_layer: &CliLayer,
    process_local_json: bool,
    printer: Printer,
) -> Result<()> {
    require_no_json_override(process_local_json)?;
    let s = Styles::detect_stderr();
    let ctx = CommandContext::for_target(&args.target, printer, cli.clone(), cli_layer)?;
    let server = ctx.server().await?;
    let base_url = args.base_url.clone();
    let credential = if args.api_key_stdin {
        provider_auth::authenticate_provider_with_api_key_source_and_base_url(
            args.provider,
            provider_auth::ApiKeySource::Stdin,
            base_url.as_deref(),
            &s,
            printer,
        )
        .await?
    } else if let Some(key) = args.api_key {
        provider_auth::authenticate_provider_with_api_key_source_and_base_url(
            args.provider,
            provider_auth::ApiKeySource::Inline(key),
            base_url.as_deref(),
            &s,
            printer,
        )
        .await?
    } else {
        provider_auth::authenticate_provider(args.provider, &s, printer).await?
    };
    let mut credential = credential;
    if let Some(url) = base_url {
        credential.base_url = Some(url);
    }
    let credential_id = credential_id_for(&credential).map_err(anyhow::Error::msg)?;
    let value = serde_json::to_string(&credential)?;

    {
        let path = legacy_env::legacy_env_file_path();
        if path.exists() {
            fabro_util::printerr!(
                printer,
                "  Warning: {} is no longer read by fabro server. Re-enter credentials with `fabro provider login`.",
                path.display()
            );
        }
    }

    server
        .api()
        .create_secret()
        .body(types::CreateSecretRequest {
            name: credential_id.clone(),
            value,
            type_: types::SecretType::Credential,
            description: None,
        })
        .send()
        .await?;
    fabro_util::printerr!(
        printer,
        "  {} Saved {}",
        s.green.apply_to("✔"),
        credential_id
    );
    Ok(())
}
