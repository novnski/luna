//! A minimal external client: it depends on `luna-sdk` alone, never on the
//! engine. Rename one task on the local engine and print its receipt:
//!
//! `scripts/cargo.sh run --locked -p luna-sdk --example rename_task -- <ipc-port> <task-id> <title>`

use std::time::Duration;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(port), Some(task_id), Some(title)) = (args.next(), args.next(), args.next()) else {
        return Err("usage: rename_task <ipc-port> <task-id> <title>".into());
    };
    let (client, engine) =
        luna_sdk::Client::connect_local(port.parse()?, Duration::from_secs(5)).await?;
    let actor = luna_sdk::Actor::new(engine.device_id, "luna-sdk-example");
    let receipt = client.rename_task(actor, &task_id, &title).await?;
    // A refusal is a receipt: `conflict` with a reason, not an error.
    println!("{}", serde_json::to_string_pretty(&receipt)?);
    Ok(())
}
