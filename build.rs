fn main() {
    embed_resource::compile("resource/resource.rc", embed_resource::NONE)
        .manifest_required()
        .unwrap();
    println!("cargo:rerun-if-changed=resource/resource.rc");
    println!("cargo:rerun-if-changed=resource/FindWinPEProfilePath.exe.manifest");
}
