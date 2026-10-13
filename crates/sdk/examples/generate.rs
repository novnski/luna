//! Regenerate the SDK manifest and the TypeScript SDK's generated types:
//! `scripts/cargo.sh run --locked -p luna-sdk --features schema --example generate`.

fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (path, contents) in luna_sdk::schema::files() {
        let target = root.join(path);
        std::fs::create_dir_all(target.parent().expect("file has a parent")).expect("create dir");
        std::fs::write(&target, contents).expect("write generated file");
        println!("{path}");
    }
}
