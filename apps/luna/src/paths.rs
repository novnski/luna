//! Application storage paths. Provider credentials keep their own locations.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Bind an implicit local CLI attachment to the exact canonical data root.
pub async fn verify_local_owner(client: &luna_rpc::RpcClient, root: &Path) -> anyhow::Result<()> {
    let expected = luna_engine::local_root_claim(root)?;
    let reply: serde_json::Value = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        client.call_as(
            luna_rpc::methods::LOCAL_OWNER_ROOT_CLAIM,
            serde_json::json!({}),
        ),
    )
    .await
    .map_err(|_| anyhow::anyhow!("local LUNA root claim timed out"))??;
    anyhow::ensure!(
        reply.get("claim").and_then(|value| value.as_str()) == Some(expected.as_str()),
        "local listener belongs to another LUNA data root"
    );
    Ok(())
}

pub fn data_dir() -> PathBuf {
    resolve_data_dir(|name| std::env::var_os(name))
}

fn resolve_data_dir(mut env: impl FnMut(&str) -> Option<OsString>) -> PathBuf {
    if let Some(dir) = env("LUNA_DATA_DIR") {
        return PathBuf::from(dir);
    }
    let home = PathBuf::from(env("HOME").expect("HOME not set"));
    home.join(".luna")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(vars: &[(&str, &str)]) -> PathBuf {
        resolve_data_dir(|name| {
            vars.iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| value.into())
        })
    }

    #[test]
    fn explicit_data_dir_needs_no_home() {
        assert_eq!(
            resolve(&[("LUNA_DATA_DIR", "custom data")]),
            PathBuf::from("custom data")
        );
    }

    #[test]
    fn default_is_home_luna_and_explicit_data_dir_wins() {
        assert_eq!(
            resolve(&[("HOME", "/Users/test")]),
            PathBuf::from("/Users/test/.luna")
        );
        assert_eq!(
            resolve(&[("HOME", "/Users/test"), ("LUNA_DATA_DIR", "/tmp/luna")]),
            PathBuf::from("/tmp/luna")
        );
    }
}
