//! Integration-style tests, factored into the crate itself rather than living under a separate
//! `tests/` directory: `dtb-ke-debugger` is a bin-only crate (no `[lib]` target), and a real
//! integration test under `tests/` compiles as its own, separate crate that has no way to reach a
//! binary's private modules. Folding them in here (as a `#[cfg(test)]`-gated submodule of the bin
//! itself, declared from `main.rs`) is what lets `crate::resolve::Resolver` and friends resolve at
//! all, while keeping each scenario in its own file rather than one giant module.

mod git_network;
mod hints;
mod native;
mod progress;
mod raw;
mod real_server;
mod server;
