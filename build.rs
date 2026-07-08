fn main() {
    println!("cargo:rerun-if-changed=src/shaders/pipeline.wgsl");
}
