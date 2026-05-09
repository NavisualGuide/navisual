// Copyright (c) 2024-2026 Jin Fu
// Licensed under the Functional Source License, Version 1.1 (Apache 2.0).
// See the LICENSE file in the root of this repository for complete details.

// Prevents additional console window on Windows in release, DO NOT REMOVE!!
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    navisual_backend_lib::run()
}
