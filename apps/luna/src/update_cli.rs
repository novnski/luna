//! LUNA update entrypoint.
//!
//! The fork intentionally has no release feed yet. Keeping the command as an
//! explicit refusal prevents source and development builds from consuming or
//! applying Luna artifacts while preserving a stable CLI surface for a future
//! owned, signed channel.

pub async fn update(_edge_url: &str, _check_only: bool) -> anyhow::Result<()> {
    anyhow::bail!("LUNA updates are disabled until an owned, signed release feed is configured")
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn refuses_unowned_update_feed() {
        let error = super::update("https://edge.luna.invalid", false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("updates are disabled"));
    }
}
