use anyhow::Context;
use anyhow::Result;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

mod cli;
mod codex_config;
mod fs_sync;
mod locale;
mod output;
mod rollout;
mod service;
mod state_db;
mod sync;
#[cfg(test)]
mod tests;
mod watch;

use cli::BucketCommand;
use cli::Command;
use cli::parse_cli;
use cli::validate_profile_override;
use cli::validate_provider_override;
use locale::Locale;
use locale::detect_locale;
use output::bucket_switch_complete_title;
use output::cli_status_command;
use output::current_exe_error;
use output::install_next_steps;
use output::launchd_plist_message;
use output::next_steps_heading;
use output::no_launchd_plist_message;
use output::print_bucket_prepare_summary;
use output::print_dry_run_summary;
use output::print_install_service_summary;
use output::print_status;
use output::print_sync_summary;
use output::prune_cancelled;
use output::prune_complete;
use output::prune_confirm_prompt;
use output::prune_skipped_warning;
use output::restore_cancelled;
use output::restore_complete;
use output::restore_confirm_prompt;
use output::restore_list_title;
use output::restore_no_backups;
use output::run_status_next_step;
use output::sync_complete_title;
use output::uninstall_launchd_done;
use rollout::RolloutProgressConfig;
use rollout::RolloutScope;
use rollout::prepare_bucket_padding;
use state_db::list_backups;
use state_db::prune_backups;
use state_db::restore_sqlite_from_backup;
use sync::collect_status;
use sync::dry_run_sync;
use sync::reconcile_once_with_backup_and_padding;
use sync::reconcile_once_with_backup_progress;
use watch::run_watch;

fn main() -> Result<()> {
    let locale = detect_locale();
    let cli = parse_cli(locale)?;
    validate_provider_override(locale, cli.provider.as_deref())?;
    validate_profile_override(locale, cli.profile.as_deref())?;
    let codex_home = cli.codex_home.unwrap_or_else(default_codex_home);

    match cli.command {
        Command::Status => {
            let summary =
                collect_status(&codex_home, cli.provider.as_deref(), cli.profile.as_deref())?;
            print_status(locale, &summary);
        }
        Command::Sync {
            sqlite_only,
            dry_run,
        } => {
            let rollout_scope = if sqlite_only {
                RolloutScope::None
            } else {
                RolloutScope::AllRows
            };
            if dry_run {
                let summary = dry_run_sync(
                    &codex_home,
                    cli.provider.as_deref(),
                    cli.profile.as_deref(),
                    rollout_scope,
                )?;
                print_dry_run_summary(locale, &summary);
            } else {
                let progress = if sqlite_only {
                    None
                } else {
                    Some(RolloutProgressConfig { locale })
                };
                let summary = reconcile_once_with_backup_progress(
                    &codex_home,
                    cli.provider.as_deref(),
                    cli.profile.as_deref(),
                    rollout_scope,
                    progress,
                )?;
                print_sync_summary(locale, sync_complete_title(locale), &summary);
            }
        }
        Command::Bucket { command } => match command {
            BucketCommand::Prepare { padding_bytes } => {
                let summary =
                    prepare_bucket_padding(&codex_home, cli.profile.as_deref(), padding_bytes)?;
                print_bucket_prepare_summary(locale, &summary);
            }
            BucketCommand::Switch {
                target_provider,
                padding_bytes,
            } => {
                validate_provider_override(locale, target_provider.as_deref())?;
                let provider = match target_provider {
                    Some(provider) => Some(provider),
                    None => cli.provider.clone(),
                };
                let summary = reconcile_once_with_backup_and_padding(
                    &codex_home,
                    provider.as_deref(),
                    cli.profile.as_deref(),
                    RolloutScope::AllRows,
                    padding_bytes,
                    Some(RolloutProgressConfig { locale }),
                )?;
                print_sync_summary(locale, bucket_switch_complete_title(locale), &summary);
            }
        },
        Command::Watch {
            poll_interval_ms,
            sqlite_only,
        } => {
            run_watch(
                locale,
                &codex_home,
                cli.provider.clone(),
                cli.profile.clone(),
                if sqlite_only {
                    RolloutScope::None
                } else {
                    RolloutScope::MismatchedRows
                },
                Duration::from_millis(poll_interval_ms),
            )?;
        }
        Command::PrintServiceConfig { poll_interval_ms } => {
            let exe_path = std::env::current_exe().context(current_exe_error(locale))?;
            let config = service::render_service_config(
                exe_path.as_path(),
                &codex_home,
                cli.provider.as_deref(),
                cli.profile.as_deref(),
                Duration::from_millis(poll_interval_ms),
            )?;
            println!("{config}");
        }
        Command::InstallService { poll_interval_ms } => {
            install_service(
                locale,
                &codex_home,
                cli.provider.as_deref(),
                cli.profile.as_deref(),
                Duration::from_millis(poll_interval_ms),
            )?;
        }
        Command::UninstallService => {
            uninstall_service(locale, &codex_home)?;
        }
        Command::Restore {
            backup_path,
            latest,
            force,
        } => {
            run_restore(
                locale,
                &codex_home,
                cli.profile.as_deref(),
                backup_path,
                latest,
                force,
            )?;
        }
        Command::PruneBackups { keep, force } => {
            run_prune_backups(locale, &codex_home, cli.profile.as_deref(), keep, force)?;
        }
    }

    Ok(())
}

fn default_codex_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".codex")
}

fn install_service(
    locale: Locale,
    codex_home: &Path,
    provider_override: Option<&str>,
    profile_override: Option<&str>,
    poll_interval: Duration,
) -> Result<()> {
    let exe_path = std::env::current_exe().context(current_exe_error(locale))?;
    let summary = service::install_service(
        exe_path.as_path(),
        codex_home,
        provider_override,
        profile_override,
        poll_interval,
    )?;

    print_install_service_summary(locale, codex_home, poll_interval, &summary);
    println!();
    println!("{}", next_steps_heading(locale));
    for line in install_next_steps(
        locale,
        exe_path.as_path(),
        codex_home,
        provider_override,
        profile_override,
        summary.manager,
    )? {
        println!("{line}");
    }
    Ok(())
}

fn uninstall_service(locale: Locale, codex_home: &Path) -> Result<()> {
    let service_status = service::current_service_status()?;
    if service_status.installed {
        let config_path = service::uninstall_service()?;
        println!("{}", uninstall_launchd_done(locale));
        println!("{}", launchd_plist_message(locale, &config_path));
        println!();
        println!("{}", next_steps_heading(locale));
        println!(
            "{}",
            run_status_next_step(
                locale,
                &cli_status_command(std::env::current_exe()?, codex_home, None, None)
            )
        );
    } else {
        println!(
            "{}",
            no_launchd_plist_message(locale, &service_status.config_path)
        );
    }
    Ok(())
}

fn run_restore(
    locale: Locale,
    codex_home: &Path,
    profile_override: Option<&str>,
    backup_path: Option<PathBuf>,
    latest: bool,
    force: bool,
) -> Result<()> {
    use crate::codex_config::resolve_sqlite_path;

    let sqlite_path = resolve_sqlite_path(codex_home, profile_override)?;
    let backups = list_backups(&sqlite_path)?;

    let chosen_path = if let Some(path) = backup_path {
        path
    } else if latest {
        let entry = backups
            .iter()
            .find(|e| e.timestamp_ms.is_some())
            .ok_or_else(|| anyhow::anyhow!(restore_no_backups(locale)))?;
        entry.path.clone()
    } else {
        if backups.is_empty() {
            println!("{}", restore_no_backups(locale));
            return Ok(());
        }
        println!("{}", restore_list_title(locale));
        for entry in &backups {
            let name = entry
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");
            if entry.timestamp_ms.is_some() {
                println!("  {name}");
            } else {
                println!("  {name}  (unparseable timestamp)");
            }
        }
        return Ok(());
    };

    if !force {
        print!("{}", restore_confirm_prompt(locale, &chosen_path));
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("{}", restore_cancelled(locale));
            return Ok(());
        }
    }

    restore_sqlite_from_backup(&sqlite_path, &chosen_path)?;
    println!("{}", restore_complete(locale, &chosen_path));
    Ok(())
}

fn run_prune_backups(
    locale: Locale,
    codex_home: &Path,
    profile_override: Option<&str>,
    keep: usize,
    force: bool,
) -> Result<()> {
    use crate::codex_config::resolve_sqlite_path;

    let sqlite_path = resolve_sqlite_path(codex_home, profile_override)?;
    let backups = list_backups(&sqlite_path)?;

    for entry in &backups {
        if entry.timestamp_ms.is_none() {
            let name = entry
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("?");
            eprintln!("{}", prune_skipped_warning(locale, name));
        }
    }

    let parseable: Vec<_> = backups
        .iter()
        .filter(|e| e.timestamp_ms.is_some())
        .collect();
    if parseable.len() <= keep {
        println!("{}", prune_complete(locale, 0, parseable.len()));
        return Ok(());
    }

    let to_delete_count = parseable.len() - keep;
    if !force {
        print!("{}", prune_confirm_prompt(locale, to_delete_count));
        std::io::stdout().flush()?;
        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;
        if !input.trim().eq_ignore_ascii_case("y") {
            println!("{}", prune_cancelled(locale));
            return Ok(());
        }
    }

    let (deleted, kept) = prune_backups(&backups, keep)?;
    println!("{}", prune_complete(locale, deleted, kept));
    Ok(())
}
