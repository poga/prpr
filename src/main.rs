fn main() {
    println!("pprr v{}", env!("CARGO_PKG_VERSION"));
    let _ = pprr::config::default_window_size();
}
