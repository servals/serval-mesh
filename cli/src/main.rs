#![forbid(unsafe_code)]
#![deny(future_incompatible)]
#![warn(
    missing_debug_implementations,
    rust_2018_idioms,
    trivial_casts,
    unused_qualifications
)]
/// Pounce is a CLI tool that interacts with a running serval agent daemon via
/// its HTTP API. It discovers running agents via mDNS advertisement.
use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};

use owo_colors::OwoColorize;
use thousands::Separable;
use tokio::runtime::Runtime;
use uuid::Uuid;

use std::fs::File;
use std::io::prelude::*;
use std::io::BufReader;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use utils::mdns::discover_service;

#[derive(Parser, Debug)]
#[clap(name = "pounce 🐈", version)]
/// A structure defining arguments implemented via `clap` derive macros.
struct Args {
    #[clap(
        short,
        parse(from_occurrences),
        help = "Pass -v or -vv to increase verbosity"
    )]
    verbose: u64,
    #[clap(subcommand)]
    cmd: Command,
}

#[derive(Clone, Debug, Subcommand)]
pub enum Command {
    /// Run the specified WASM binary.
    #[clap(display_order = 1)]
    Run {
        /// A descriptive name for the job
        #[clap(long, short)]
        name: Option<String>,
        /// A description for the job
        #[clap(long, short)]
        description: Option<String>,
        /// The file containing the wasm binary to run
        #[clap(value_name = "WASM BINARY")]
        binary_file: PathBuf,
        /// Path to a file to pass to the binary; omit to read from stdin (if present)
        #[clap(value_name = "OPTIONAL INPUT TO WASM BINARY")]
        input_file: Option<PathBuf>,
        /// Path to write the output of the job. Omit to write to stdout.
        output_file: Option<PathBuf>,
    },
    /// Get the status of a job in progress.
    #[clap(display_order = 2)]
    Status { id: Uuid },
    /// Get results for a job run, given its ID.
    #[clap(display_order = 3)]
    Results { id: Uuid },
    /// Get full job run history from the running process.
    #[clap(display_order = 4)]
    History,
}

static SERVAL_NODE_URL: Mutex<Option<String>> = Mutex::new(None);

/// Convenience function to build urls repeatably.
fn build_url(path: String) -> String {
    let baseurl = SERVAL_NODE_URL.lock().unwrap();
    let baseurl = baseurl
        .as_ref()
        .expect("build_url called while SERVAL_NODE_URL is None");
    format!("{baseurl}/v1/{path}")
}

/// Convenience function to read an input wasm binary either from a pathbuf or from stdin.
fn read_file_or_stdin(maybepath: Option<PathBuf>) -> Result<Vec<u8>, anyhow::Error> {
    // TODO This implementation should become a streaming implementation.
    let mut buf: Vec<u8> = Vec::new();
    if let Some(fpath) = maybepath {
        return read_file(fpath);
    }

    if atty::is(atty::Stream::Stdin) {
        return Ok(buf);
    }

    let mut reader = BufReader::new(std::io::stdin());
    reader.read_to_end(&mut buf)?;

    Ok(buf)
}

fn read_file(path: PathBuf) -> Result<Vec<u8>, anyhow::Error> {
    // TODO This implementation should become a streaming implementation.
    let mut buf: Vec<u8> = Vec::new();
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    reader.read_to_end(&mut buf)?;

    Ok(buf)
}

/// Post a WASM executable to a waiting agent to run.
fn run(
    name: Option<String>,
    description: Option<String>,
    binarypath: PathBuf,
    maybeinputpath: Option<PathBuf>,
    maybeoutputpath: Option<PathBuf>,
) -> Result<()> {
    let binary_bytes = read_file(binarypath.clone())?;
    if binary_bytes.is_empty() {
        return Err(anyhow!("no executable data read!"));
    }

    let input_bytes = read_file_or_stdin(maybeinputpath)?;
    let binary_payload_size = binary_bytes.len() + input_bytes.len();
    let binary_part = reqwest::blocking::multipart::Part::bytes(binary_bytes);
    let input_part = reqwest::blocking::multipart::Part::bytes(input_bytes);

    let name = name.unwrap_or_else(|| {
        // use the filename component of binarypath, e.g. /foo/bar.wasm -> bar.wasm
        binarypath
            .file_name()
            .and_then(|name| name.to_str())
            .map(|z| z.to_string())
            .unwrap_or_else(|| "unnamed".to_string())
    });
    let description = description.unwrap_or_else(|| "posted via command-line".to_string());

    println!(
        "Sending {} ({} bytes for binary + payload) to serval agent...",
        name.blue().bold(),
        binary_payload_size.separate_with_commas(),
    );

    let envelope = serde_json::json!({
        "id": &Uuid::new_v4().to_string(),
        "name": &name,
        "description": &description
    });
    let envelope_part = reqwest::blocking::multipart::Part::text(envelope.to_string());

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let form = reqwest::blocking::multipart::Form::new()
        .part("envelope", envelope_part)
        .part("executable", binary_part)
        .part("input", input_part);

    let url = build_url("jobs".to_string());
    let response = client.post(url).multipart(form).send()?;

    let response_body = response.bytes()?;
    log::info!("response body read; length={}", response_body.len());
    match maybeoutputpath {
        Some(outputpath) => {
            eprintln!("Writing output to {outputpath:?}");
            let mut f = File::create(&outputpath)?;
            f.write_all(&response_body)?;
        }
        None => {
            if atty::is(atty::Stream::Stdin) && String::from_utf8(response_body.to_vec()).is_err() {
                eprintln!("Response is non-printable binary data; redirect output to a file or provide an output filename to retrieve it.");
            } else {
                eprintln!("----------");
                std::io::stdout().write_all(&response_body)?;
                eprintln!("----------");
            };
        }
    }

    Ok(())
}

/// Get a job's status from a serval agent node.
fn status(id: Uuid) -> Result<()> {
    let url = build_url(format!("jobs/{id}/status"));
    let response = reqwest::blocking::get(url)?;
    let body: serde_json::Map<String, serde_json::Value> = response.json()?;
    println!("{}", serde_json::to_string_pretty(&body)?);

    Ok(())
}

/// Get a job's results from a serval agent node.
fn results(id: Uuid) -> Result<()> {
    let url = build_url(format!("jobs/{id}/results"));
    let response = reqwest::blocking::get(url)?;
    let body: serde_json::Map<String, serde_json::Value> = response.json()?;
    println!("{}", serde_json::to_string_pretty(&body)?);

    Ok(())
}

/// Get in-memory history from an agent node.
fn history() -> Result<()> {
    let url = build_url("monitor/history".to_string());
    let response = reqwest::blocking::get(url)?;
    let body: serde_json::Map<String, serde_json::Value> = response.json()?;
    println!("{}", serde_json::to_string_pretty(&body)?);

    Ok(())
}

fn blocking_maybe_discover_service_url(
    service_type: &str,
    env_var_override_name: &str,
) -> Result<String> {
    if let Ok(override_url) = std::env::var(env_var_override_name) {
        return Ok(override_url);
    }

    log::info!("Looking for {service_type} node on the local network...");

    let Ok(info) = Runtime::new().unwrap().block_on(discover_service(service_type)) else {
        return Err(anyhow!(format!(
            "Failed to discover {service_type} node on the local network"
        )));
    };

    let Some(addr) = info.get_addresses().iter().next() else {
        // this should not ever happen, but computers
        return Err(anyhow!(format!(
            "Discovered a node that has no addresses",
        )));
    };

    let port = info.get_port();
    return Ok(format!("http://{addr}:{port}"));
}

/// Parse command-line arguments and act.
fn main() -> Result<()> {
    let args = Args::parse();

    loggerv::Logger::new()
        .verbosity(args.verbose) // if -v not passed, our default level is WARN
        .line_numbers(false)
        .module_path(true)
        .colors(true)
        .init()
        .unwrap();

    let baseurl = blocking_maybe_discover_service_url("_serval_daemon", "SERVAL_NODE_URL")?;
    SERVAL_NODE_URL.lock().unwrap().replace(baseurl);

    match args.cmd {
        Command::Run {
            name,
            description,
            binary_file,
            input_file,
            output_file,
        } => {
            // If people provide - as the filename, interpret that as stdin/stdout
            let input_file = input_file.filter(|p| p != &PathBuf::from("-"));
            let output_file = output_file.filter(|p| p != &PathBuf::from("-"));
            run(name, description, binary_file, input_file, output_file)?;
        }
        Command::Results { id } => results(id)?,
        Command::Status { id } => status(id)?,
        Command::History => history()?,
    };

    Ok(())
}
