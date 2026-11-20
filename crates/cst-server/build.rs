fn main() {
    println!("cargo:rerun-if-env-changed=CST_BUILD_VERSION");
}
