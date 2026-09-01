// `option_env!("LAYSI_BUILD")` reads the variable at compile time but records
// no dependency on it, and the release image keeps cargo's target directory on
// a cache mount. Without this line the next tag would reuse an object file
// with the previous tag compiled into it.
fn main() {
    println!("cargo:rerun-if-env-changed=LAYSI_BUILD");
}
