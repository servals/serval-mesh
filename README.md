# serval-mesh

[![Main branch checks](https://github.com/servals/serval-mesh/actions/workflows/main.yaml/badge.svg)](https://github.com/servals/serval-mesh/actions/workflows/main.yaml)

This monorepo contains the source for the various components of the Serval mesh, intended to run on any host where you want to run WASM payloads. As of December 2022, this project is in very early stages of development and is changing rapidly.

The repo is a Rust workspace containing the following members:

- `agent`: a daemon that listens on a port for incoming HTTP requests with payloads to run
- `engine`: a library for the [wasmtime](https://lib.rs/crates/wasmtime) glue; in early stages
- `cli`: a command-line interface (called `serval` when built) for controlling the mesh and creating WASM jobs
- `utils`: a library for code we use in several places
- `storage`: temporarily a separate service during MVP/demo period, this stores blobs for the Serval mesh. This functionality will eventually be integrated into the Serval agent.
- `queue`: temporarily a separate service during MVP/demo period, this is manages the job queue for the Serval mesh. This functionality will eventually be integrated into the Serval agent.
- `test-runner`: a CLI to execute a WASM payload once, useful for developing the engine

## Local development

This is a Rust project. If you do not have the rust compiler available, install it with [rustup](https://rustup.rs).

A [justfile](https://just.systems) is provided for your convenience. It defines these recipes:

```console
$ just -l
Available recipes:
    build         # Build all targets in debug mode
    ci            # Run the same checks we run in CI
    dance         # Everyone loves Lady Gaga, right?
    help          # List available recipes
    install-tools # Cargo install required tools like `nextest`
    lint          # Lint and automatically fix what we can fix
    release       # Build all targets in release mode
    security      # Get security advisories from cargo-deny
    test          # Run tests with nextest
```

## LICENSE

[BSD-2-Clause-Patent](./LICENSE)
