//! CORE-00 feasibility only. Failure contracts precede this code in failures.md.
use anyhow::{Result, ensure};
use serde_json::json;
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use wasmtime::{Caller, Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder};

struct State {
    limits: StoreLimits,
    active: Arc<AtomicUsize>,
    effects: Arc<AtomicUsize>,
}

struct HostLease(Arc<AtomicUsize>);
impl Drop for HostLease {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn store(
    engine: &Engine,
    active: &Arc<AtomicUsize>,
    effects: &Arc<AtomicUsize>,
) -> Result<Store<State>> {
    let mut store = Store::new(
        engine,
        State {
            limits: StoreLimitsBuilder::new()
                .memory_size(16 * 1024 * 1024)
                .memories(1)
                .instances(1)
                .table_elements(1024)
                .build(),
            active: active.clone(),
            effects: effects.clone(),
        },
    );
    store.limiter(|state| &mut state.limits);
    store.set_fuel(1_000_000_000)?;
    store.fuel_async_yield_interval(Some(10_000))?;
    Ok(store)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let output = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: luna-core-runtime-probe OUTPUT.json"))?;
    let start = Instant::now();
    let mut config = Config::new();
    config.consume_fuel(true);
    let engine = Engine::new(&config)?;
    let active = Arc::new(AtomicUsize::new(0));
    let effects = Arc::new(AtomicUsize::new(0));
    let entered = Arc::new(AtomicUsize::new(0));
    let host_entries = entered.clone();
    let mut linker = Linker::<State>::new(&engine);
    linker.func_wrap_async(
        "luna_probe_v1",
        "wait",
        move |caller: Caller<'_, State>, (): ()| {
            let active = caller.data().active.clone();
            let effects = caller.data().effects.clone();
            let entries = host_entries.clone();
            Box::new(async move {
                entries.fetch_add(1, Ordering::SeqCst);
                active.fetch_add(1, Ordering::SeqCst);
                let _lease = HostLease(active);
                tokio::time::sleep(Duration::from_secs(1)).await;
                effects.fetch_add(1, Ordering::SeqCst);
            })
        },
    )?;
    let sources = [
        (
            "ok",
            "(module (func (export \"run\") (result i32) i32.const 42))",
        ),
        (
            "loop",
            "(module (func (export \"run\") (result i32) (loop br 0) i32.const 0))",
        ),
        (
            "host_wait",
            "(module (import \"luna_probe_v1\" \"wait\" (func $wait)) (func (export \"run\") (result i32) call $wait i32.const 1))",
        ),
        (
            "memory",
            "(module (memory 1) (func (export \"run\") (result i32) i32.const 256 memory.grow))",
        ),
    ];
    let mut samples = Vec::new();
    let mut cold = Vec::new();
    for (name, source) in sources {
        let compilation = Instant::now();
        let module = Module::new(&engine, source)?;
        let compile_ms = compilation.elapsed().as_secs_f64() * 1000.;
        // Repeated fresh stores prevent a previous trap from proving the next case.
        for repetition in 0..20 {
            let mut store = store(&engine, &active, &effects)?;
            let creation = Instant::now();
            let instance = linker.instantiate_async(&mut store, &module).await?;
            let instantiate_ms = creation.elapsed().as_secs_f64() * 1000.;
            if repetition == 0 {
                cold.push(json!({"guest": name, "compile_ms": compile_ms, "instantiate_ms": instantiate_ms}));
            }
            let run = instance.get_typed_func::<(), i32>(&mut store, "run")?;
            let call = Instant::now();
            let result =
                tokio::time::timeout(Duration::from_millis(50), run.call_async(&mut store, ()))
                    .await;
            let elapsed_ms = call.elapsed().as_secs_f64() * 1000.;
            let observed = match name {
                "ok" => {
                    ensure!(result?? == 42, "wrong guest result");
                    "42"
                }
                "memory" => {
                    ensure!(result?? == -1, "memory bound not enforced");
                    "growth denied"
                }
                _ => {
                    ensure!(
                        result.is_err(),
                        "{name} was not cancelled by the host deadline"
                    );
                    ensure!(
                        elapsed_ms < 150.,
                        "{name} cancellation exceeded ceiling: {elapsed_ms} ms"
                    );
                    "deadline cancelled"
                }
            };
            ensure!(active.load(Ordering::SeqCst) == 0, "host operation leaked");
            ensure!(
                effects.load(Ordering::SeqCst) == 0,
                "cancelled host operation committed an effect"
            );
            samples.push(json!({"guest": name, "repetition": repetition, "elapsed_ms": elapsed_ms, "observed": observed, "active_host_calls": active.load(Ordering::SeqCst)}));
        }
    }
    let denied = Module::new(
        &engine,
        "(module (import \"wasi_snapshot_preview1\" \"fd_write\" (func)))",
    )?;
    let error = linker
        .instantiate_async(&mut store(&engine, &active, &effects)?, &denied)
        .await
        .err()
        .ok_or_else(|| anyhow::anyhow!("ambient import unexpectedly allowed"))?;
    // Give a wrongly detached host operation time to commit its forbidden effect.
    tokio::time::sleep(Duration::from_millis(1100)).await;
    ensure!(
        entered.load(Ordering::SeqCst) == 20,
        "host waits were not actually entered"
    );
    ensure!(
        active.load(Ordering::SeqCst) == 0 && effects.load(Ordering::SeqCst) == 0,
        "late host operation effect"
    );
    let recovery = Module::new(&engine, sources[0].1)?;
    let mut recovered_store = store(&engine, &active, &effects)?;
    let recovered = linker
        .instantiate_async(&mut recovered_store, &recovery)
        .await?;
    ensure!(
        recovered
            .get_typed_func::<(), i32>(&mut recovered_store, "run")?
            .call_async(&mut recovered_store, ())
            .await?
            == 42,
        "host failed to recover"
    );
    std::fs::write(
        output,
        serde_json::to_vec_pretty(&json!({
            "passed": true, "runtime": "wasmtime 49.0.2", "boundary": "standalone real Wasmtime, no WASI imports",
            "fuel_yield_interval": 10000, "deadline_ms": 50, "maximum_return_ms": 150,
            "linear_memory_limit_bytes": 16 * 1024 * 1024, "cold": cold, "samples": samples,
        "denied_import": error.to_string(), "host_calls_entered": entered.load(Ordering::SeqCst), "active_host_calls": active.load(Ordering::SeqCst), "late_effects": effects.load(Ordering::SeqCst),
            "recovered_result": 42, "total_ms": start.elapsed().as_secs_f64() * 1000.
        }))?,
    )?;
    Ok(())
}
