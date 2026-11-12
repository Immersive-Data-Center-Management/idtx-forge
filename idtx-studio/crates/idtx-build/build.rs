fn main() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
    idtx_build_metadata::emit_dependency_versions(root).expect("resolve cook dependency pins");
}
