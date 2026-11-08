//! `cargo run -p aun-ffi --features cli --bin uniffi-bindgen -- generate --library <lib> --language swift …`
fn main() {
    uniffi::uniffi_bindgen_main()
}
