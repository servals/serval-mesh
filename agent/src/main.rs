#![forbid(unsafe_code)]
#![deny(future_incompatible)]
#![warn(
    missing_debug_implementations,
    rust_2018_idioms,
    trivial_casts,
    unused_qualifications
)]

use anyhow::Result;
use axum::{
    extract::DefaultBodyLimit,
    middleware::{self},
    routing::{get, head, post, put},
    Router, Server,
};
use dotenvy::dotenv;
use engine::ServalEngine;
use utils::{mdns::advertise_service, networking::find_nearest_port};
use uuid::Uuid;

use std::{net::SocketAddr, process};
use std::{path::PathBuf, sync::Arc};

mod api;
use crate::api::*;

mod structures;
use crate::structures::*;

#[tokio::main]
async fn main() -> Result<()> {
    let did_find_dotenv = dotenv().ok().is_some();
    if cfg!(debug_assertions) && !did_find_dotenv {
        println!("Debug-only warning: no .env file found to configure logging; all logging will be disabled. Add RUST_LOG=info to .env to see logging.");
    }
    env_logger::init();

    let host = std::env::var("HOST").unwrap_or_else(|_| "0.0.0.0".to_string());
    let storage_role = match &std::env::var("STORAGE_ROLE").unwrap_or_else(|_| "auto".to_string())[..]
    {
        "always" => true,
        "auto" => {
            // todo: add some sort of heuristic to determine whether we should be a storage node
            // for now, don't be a storage node unless explicitly asked to be; this should change
            // once we have distributed storage rather than a single-node temporary hack.
            false
        }
        "never" => false,
        _ => {
            log::warn!(
                "Invalid value for STORAGE_ROLE environment variable; defaulting to 'never'"
            );
            false
        }
    };
    let blob_path = if storage_role {
        Some(
            std::env::var("BLOB_STORE")
                .map(PathBuf::from)
                .unwrap_or_else(|_| std::env::temp_dir().join("serval_storage")),
        )
    } else {
        None
    };
    let should_run_jobs = match &std::env::var("RUNNER_ROLE").unwrap_or_else(|_| "auto".to_string())
        [..]
    {
        "always" => {
            if !ServalEngine::is_available() {
                log::error!("RUNNER_ROLE environment variable is set to 'always', but this platform is not supported by our WASM engine.");
                process::exit(1)
            }
            true
        }
        "auto" => ServalEngine::is_available(),
        "never" => false,
        _ => {
            log::warn!("Invalid value for RUNNER_ROLE environment variable; defaulting to 'never'");
            false
        }
    };

    let extensions_path = std::env::var("EXTENSIONS_PATH").ok().map(PathBuf::from);

    let instance_id = Uuid::new_v4();
    let state = Arc::new(RunnerState::new(
        instance_id,
        blob_path.clone(),
        extensions_path.clone(),
        should_run_jobs,
    )?);
    log::info!(
        "agent configured with storage={} and run-jobs={}",
        state.has_storage,
        state.should_run_jobs
    );

    const MAX_BODY_SIZE_BYTES: usize = 100 * 1024 * 1024;
    let app = Router::new()
        .route("/monitor/ping", get(ping))
        .route("/monitor/status", get(v1::jobs::monitor_status))
        // begin optional endpoints; these requests will be pre-empted by our
        // proxy_unavailable_services middleware if they aren't implemented by this instance.
        .route("/v1/jobs", get(v1::jobs::running)) // TODO
        .route("/v1/jobs/:name/run", post(v1::jobs::run_job)) // has an input payload; TODO options (needs design)
        .route("/v1/storage/manifests", get(v1::storage::list_manifests))
        .route("/v1/storage/manifests", post(v1::storage::store_manifest))
        .route(
            "/v1/storage/manifests/:name",
            get(v1::storage::get_manifest),
        )
        .route(
            "/v1/storage/manifests/:name",
            head(v1::storage::has_manifest),
        )
        .route(
            "/v1/storage/manifests/:name/executable/:version",
            put(v1::storage::store_executable),
        )
        .route(
            "/v1/storage/manifests/:name/executable/:version",
            get(v1::storage::get_executable),
        )
        // end optional endpoints
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            v1::proxy::proxy_unavailable_services,
        ))
        .route_layer(middleware::from_fn(clacks))
        .layer(DefaultBodyLimit::max(MAX_BODY_SIZE_BYTES))
        .with_state(state.clone());

    let predefined_port: Option<u16> = match std::env::var("PORT") {
        Ok(port_str) => port_str.parse::<u16>().ok(),
        Err(_) => None,
    };

    // Start the Axum server; this is in a loop so we can try binding more than once in case our
    // randomly-selected port number ends up conflicting with something else due to a race condition.
    let mut port: u16;
    let server: Server<_, _> = loop {
        port = predefined_port.unwrap_or_else(|| find_nearest_port(8100).unwrap());
        let addr: SocketAddr = format!("{host}:{port}").parse().unwrap();
        match axum::Server::try_bind(&addr) {
            Ok(builder) => break builder.serve(app.into_make_service()),
            Err(_) => {
                // Port number in use already, presumably
                if predefined_port.is_some() {
                    log::error!("Specified port number ({port}) is already in use; aborting");
                    process::exit(1);
                }
            }
        }
    };

    log::info!("serval agent daemon listening on {host}:{port}");
    advertise_service("serval_daemon", port, &instance_id, None)?;

    if blob_path.is_some() {
        log::info!("serval agent blob store mounted; path={blob_path:?}");
        advertise_service("serval_storage", port, &instance_id, None)?;
    }
    if let Some(extensions_path) = extensions_path {
        let extensions = &state.extensions;
        log::info!(
            "Found {} extensions at {extensions_path:?}: {:?}",
            extensions.len(),
            extensions.keys(),
        );
    }

    if should_run_jobs {
        // todo: actually start polling job queue for work to do
        log::info!("job running enabled");
        advertise_service("serval_runner", port, &instance_id, None)?;
    } else {
        log::info!("job running not enabled (or not supported)");
    }

    server.await.unwrap();
    Ok(())
}
