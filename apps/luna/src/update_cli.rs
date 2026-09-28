//! Signed GitHub Release update entrypoint.

use anyhow::{Context as _, bail};
use luna_update::{InstallKind, detect_install, fetch_latest, version_newer};

pub async fn update(edge_url: &str, check_only: bool) -> anyhow::Result<()> {
    let manifest = fetch_latest(edge_url)
        .await
        .context("checking GitHub Releases for updates")?;
    let current = luna_update::current_version();
    if !version_newer(&manifest.version, current) {
        println!(
            "LUNA {current} is up to date (latest: {}).",
            manifest.version
        );
        return Ok(());
    }
    println!(
        "LUNA {} is available (installed: {current}).",
        manifest.version
    );
    if check_only {
        return Ok(());
    }

    match detect_install() {
        InstallKind::Managed { app_root } => {
            luna_update::stage_headless(edge_url, &manifest, &app_root).await?;
            luna_update::apply_headless(&app_root, &manifest.version)?;
            println!("Installed LUNA {}. Restarting service.", manifest.version);
            luna_update::restart_service()
                .context("update installed, but service restart failed")?;
        }
        kind if kind.supports_desktop_update() => {
            #[cfg(target_os = "macos")]
            if matches!(&kind, InstallKind::MacApp { .. }) {
                ensure_no_other_mac_instance()?;
            }
            let staged = kind
                .stage_desktop(edge_url, &manifest, &crate::paths::data_dir())
                .await?;
            kind.apply_desktop(&staged)?;
            println!("Installed LUNA {}. Relaunching app.", manifest.version);
        }
        _ => bail!(
            "this installation is not update-managed; use a GitHub Release download or build from source"
        ),
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn ensure_no_other_mac_instance() -> anyhow::Result<()> {
    let executable = std::env::current_exe()?.to_string_lossy().into_owned();
    let output = std::process::Command::new("/bin/ps")
        .args(["-axo", "pid=,command="])
        .output()
        .context("checking running LUNA app processes")?;
    anyhow::ensure!(
        output.status.success(),
        "could not check running LUNA app processes"
    );
    let current_pid = std::process::id();
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let line = line.trim_start();
        let Some(split) = line.find(char::is_whitespace) else {
            continue;
        };
        let Ok(pid) = line[..split].parse::<u32>() else {
            continue;
        };
        let command = line[split..].trim_start();
        if pid != current_pid
            && command.strip_prefix(&executable).is_some_and(|tail| {
                tail.is_empty() || tail.chars().next().is_some_and(char::is_whitespace)
            })
        {
            bail!(
                "LUNA is already running from this app bundle; quit it before using the update command"
            );
        }
    }
    Ok(())
}
