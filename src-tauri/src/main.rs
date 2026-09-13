// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    // First statement in the process, so every later mark is measured against
    // something meaningful. See the `trace` module.
    media_compressor_lib::trace::mark("main");
    media_compressor_lib::run()
}
