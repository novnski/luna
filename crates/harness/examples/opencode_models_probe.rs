//! Live probe: opencode model discovery through the native driver — boots
//! `opencode serve`, reads `GET /api/model`, prints the picker catalog
//! (enabled models only). Needs OpenCode 2 on PATH (or
//! OPENCODE_EXECUTABLE); no provider auth required (the anonymous Zen tier
//! always connects).
//!
//!     cargo run -p luna-harness --example opencode_models_probe

use luna_harness::{Harness, OpencodeHarness};

#[tokio::main]
async fn main() {
    let start = std::time::Instant::now();
    match OpencodeHarness::new().models().await {
        Ok(models) => {
            for m in &models {
                eprintln!(
                    "{:40} {:28} {:?} {:?}",
                    m.id,
                    m.label,
                    m.description.as_deref().unwrap_or(""),
                    m.reasoning_levels
                );
            }
            eprintln!("--- {} models in {:?}", models.len(), start.elapsed());
            print_resolution();
        }
        Err(e) => {
            eprintln!("discovery failed: {e}");
            print_resolution();
            std::process::exit(1);
        }
    }
}

/// The binary and API contract this process used (OC-00/OC-02 evidence).
fn print_resolution() {
    let Some(resolution) = luna_harness::opencode::last_opencode_resolution() else {
        eprintln!("RESOLVED none");
        return;
    };
    let version = resolution.version.as_deref().unwrap_or("unknown");
    eprintln!(
        "RESOLVED {} version={version} source={:?}",
        resolution.path.display(),
        resolution.source
    );
    if let Some(report) = luna_harness::opencode::opencode_compat_report(version) {
        eprintln!("COMPAT {report:?}");
    }
}
