//! Regenerate the published CORE-01 contract under `crates/rpc/contract/v1/`:
//! `scripts/cargo.sh run --locked -p luna-rpc --features schema --example contract`.

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("contract/v1");
    for (path, contents) in luna_rpc::contract::files() {
        let target = root.join(path);
        std::fs::create_dir_all(target.parent().expect("file has a parent")).expect("create dir");
        std::fs::write(&target, contents).expect("write contract file");
        println!("{}", target.display());
    }
}
