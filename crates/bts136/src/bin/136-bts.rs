use bts136::{
    config::Config,
    core::messaging::LocalMessaging,
    phy::{pulse::SYMBOL_RATE_HZ, traffic::SLOT_BITS},
    radio::{PreparedRadio, duplex::Duplex, receiver, timed_tx},
};
use std::{env, error::Error, ffi::OsString, fs, path::PathBuf, process::ExitCode};

static LOGGING_INITIALIZED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);
const USAGE: &str = "Usage: 136-bts --run CONFIG.yaml [--duration-seconds N]\n       136-bts --check-startup CONFIG.yaml\n       136-bts --help\n\n--run starts the carrier and registration receiver until SIGINT or SIGTERM.\n--duration-seconds bounds a run. Handles registration, full-rate VSELP/ACELP CC1 calls and local SMS without authentication.\n--check-startup validates configuration and inactive RX/TX ownership.\nEnable web in YAML to serve the embedded carrier console and gRPC APIs.";
const USAGE_EXIT_CODE: u8 = 2;
/// Keeps the console serving the failed state long enough for connected browsers to receive it.
const FAILURE_DISPLAY_LINGER: std::time::Duration = std::time::Duration::from_secs(3);
const MAX_QUEUED_TX_BLOCKS: usize = 4;
const RX_BLOCK_MILLISECONDS: usize = 5;
const MILLISECONDS_PER_SECOND: usize = 1000;
const RX_TIMEOUT_US: i64 = 1000;
// The logo is the six-slot IS-136 frame with the DCCH on slots 1 and 4.
const BANNER_MARK: &str = "▮ ▯ ▯ ▮ ▯ ▯";
const BANNER_MARK_ANSI: &str =
    "\x1b[1;97m▮\x1b[0m \x1b[38;5;61m▮ ▮\x1b[0m \x1b[1;97m▮\x1b[0m \x1b[38;5;61m▮ ▮\x1b[0m";
const BANNER_INDENT: &str = "             ";
enum Command {
    Help,
    CheckStartup(PathBuf),
    Run(PathBuf, Option<std::time::Duration>),
}
fn command(mut args: impl Iterator<Item = OsString>) -> Option<Command> {
    let option = args.next()?;
    if option == "--help" && args.next().is_none() {
        return Some(Command::Help);
    }
    if option != "--check-startup" && option != "--run" {
        return None;
    }
    let path = PathBuf::from(args.next()?);
    if path.as_os_str().is_empty() {
        return None;
    }
    if option == "--check-startup" {
        return args.next().is_none().then_some(Command::CheckStartup(path));
    }
    let duration = match args.next() {
        None => None,
        Some(option) if option == "--duration-seconds" => {
            let seconds = args.next()?.to_str()?.parse::<u64>().ok()?;
            if seconds == 0 || args.next().is_some() {
                return None;
            }
            Some(std::time::Duration::from_secs(seconds))
        }
        _ => return None,
    };
    Some(Command::Run(path, duration))
}
fn run(
    path: PathBuf,
    duration: Option<std::time::Duration>,
    log_guard: &mut Option<tracing_appender::non_blocking::WorkerGuard>,
) -> Result<(), Box<dyn Error>> {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    use tracing_subscriber::EnvFilter;
    let config = Config::from_yaml(&fs::read_to_string(path)?)?;
    if matches!(config.logging.format, bts136::config::LogFormat::Text) {
        print_banner(&config);
    }
    let filter =
        EnvFilter::try_new(env::var("RUST_LOG").unwrap_or_else(|_| config.logging.filter.clone()))?;
    use tracing_subscriber::prelude::*;
    let (hub, interface) = bts136::operations::Hub::new(&config);
    let (writer, guard) = tracing_appender::non_blocking(std::io::stderr());
    *log_guard = Some(guard);
    match config.logging.format {
        bts136::config::LogFormat::Text => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
            .with_writer(writer)
            .finish()
            .with(hub.event_layer())
            .try_init()
            .map_err(|e| -> Box<dyn Error> { Box::new(e) })?,
        bts136::config::LogFormat::Json => tracing_subscriber::fmt()
            .json()
            .with_env_filter(filter)
            .with_writer(writer)
            .finish()
            .with(hub.event_layer())
            .try_init()
            .map_err(|e| -> Box<dyn Error> { Box::new(e) })?,
    }
    LOGGING_INITIALIZED.store(true, Ordering::Relaxed);
    let shutdown = Arc::new(AtomicBool::new(false));
    let signal = shutdown.clone();
    ctrlc::set_handler(move || signal.store(true, Ordering::Relaxed))?;
    let web = if config.web.enabled {
        let server = bts136::operations::Server::start_with_origins(
            config.web.listen,
            hub,
            config.web.allowed_origins.clone(),
        )?;
        tracing::info!(listen = %server.address(), "operations web and gRPC server started");
        Some(server)
    } else {
        None
    };
    let result = bts136::runtime::serving::run_with_interface(
        &config,
        &shutdown,
        duration,
        config.web.enabled.then_some(interface),
    );
    if let Some(server) = web {
        if result.is_err() {
            std::thread::sleep(FAILURE_DISPLAY_LINGER);
        }
        if let Err(error) = server.stop() {
            tracing::warn!(%error, "operations web server did not stop cleanly");
        }
    }
    let summary = result?;
    tracing::info!(
        submitted_slots = summary.submitted_slots,
        completed_registrations = summary.completed_registrations,
        stop_acknowledged = summary.stop_acknowledged,
        "run completed"
    );
    Ok(())
}
fn check_startup(path: PathBuf) -> Result<(), Box<dyn Error>> {
    let config = Config::from_yaml(&fs::read_to_string(&path)?)?;
    let state = config.prepare()?;
    let core = LocalMessaging::from_store(config.open_store()?)?;
    let prepared = PreparedRadio::open(&config)?;
    let settings = prepared.settings();
    let samples_per_symbol = settings.applied_tx_sample_rate_hz / SYMBOL_RATE_HZ as usize;
    let streams = prepared.into_streams(
        timed_tx::Limits {
            max_queued_blocks: MAX_QUEUED_TX_BLOCKS,
            max_block_samples: SLOT_BITS / 2 * samples_per_symbol,
        },
        receiver::Limits {
            max_block_samples: settings.applied_rx_sample_rate_hz * RX_BLOCK_MILLISECONDS
                / MILLISECONDS_PER_SECOND,
            timeout_us: RX_TIMEOUT_US,
        },
    )?;
    let mut radio = Duplex::try_new(streams, samples_per_symbol)?;
    radio.stop()?;
    println!(
        "startup_check=passed forward_hz={} reverse_hz={} tx_sample_rate_hz={} rx_sample_rate_hz={} tx_tick_rate_hz={} rx_tick_rate_hz={} subscribers={} authentication=disabled stop_acknowledged={} serving_cell=false",
        state.frequencies.forward_hz,
        state.frequencies.reverse_hz,
        settings.applied_tx_sample_rate_hz,
        settings.applied_rx_sample_rate_hz,
        settings.tx_tick_rate_hz,
        settings.rx_tick_rate_hz,
        core.registry().subscribers().len(),
        radio.stop_acknowledged()
    );
    Ok(())
}
fn print_banner(config: &Config) {
    let ansi = std::io::IsTerminal::is_terminal(&std::io::stderr());
    let (mark, name) = if ansi {
        (BANNER_MARK_ANSI, "\x1b[1m136BTS\x1b[0m")
    } else {
        (BANNER_MARK, "136BTS")
    };
    let band = match config.cell.band {
        bts136::config::ConfigBand::Cellular800 => "Cellular 800",
        bts136::config::ConfigBand::Pcs1900 => "PCS 1900",
    };
    eprintln!("{mark}  {name} {}", env!("CARGO_PKG_VERSION"));
    eprintln!(
        "{BANNER_INDENT}IS-136 cell · {band} · channel {} · SID {}",
        config.cell.channel, config.cell.sid
    );
}
fn main() -> ExitCode {
    let mut log_guard = None;
    let Some(command) = command(env::args_os().skip(1)) else {
        eprintln!("{USAGE}");
        return ExitCode::from(USAGE_EXIT_CODE);
    };
    match command {
        Command::Help => {
            println!("{USAGE}");
            ExitCode::SUCCESS
        }
        Command::Run(path, duration) => match run(path, duration, &mut log_guard) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                if LOGGING_INITIALIZED.load(std::sync::atomic::Ordering::Relaxed) {
                    tracing::error!(%error,"cell run failed");
                } else {
                    eprintln!("cell_run=failed error={error}");
                }
                ExitCode::FAILURE
            }
        },
        Command::CheckStartup(path) => match check_startup(path) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("startup_check=failed error={error}");
                ExitCode::FAILURE
            }
        },
    }
}
