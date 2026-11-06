//! gpuwatch: samples NVML inside a Modal container, detects sustained thermal
//! throttling, and reports it to a collector and to a state file.

mod rule;

use rule::{Sample, Sensitivity, Thresholds, Verdict, Window, WindowStats};
use serde::Serialize;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::process::ExitCode;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Where events go unless GPUWATCH_REPORT_URL / --report-url say otherwise;
/// the value "none" disables reporting.
const DEFAULT_REPORT_URL: &str = "https://modal-labs--gpuwatch-collector-web.modal.run/report";
/// One daemon per container: a second `watch` finds the lock held and exits.
const LOCK_FILE: &str = "/tmp/gpuwatch.lock";
const USAGE: &str = "\
gpuwatch: detect sustained GPU thermal throttling inside a Modal container

  gpuwatch check [--seconds N]      sample for N seconds (default 30), print a
                                    verdict; exit 2 when throttling, 0 otherwise
  gpuwatch watch                    sample until killed, report each event
                                    (the default when no command is given)
  gpuwatch event                    print the daemon's last event as JSON and
                                    exit 2; exit 0 with no output when none
  gpuwatch --version

Options for both:
  --sensitivity low|medium|high     preset (default medium)
  --window MINUTES                  busy minutes per window (overrides preset)
  --reduction FRACTION              mean clock reduction while thermally
                                    throttled that fires (overrides preset)
  --interval SECONDS                poll interval (default 2)
  --report-url URL                  POST each event (and each check) as JSON
                                    (default: Modal's collector; none disables)
  --state-file PATH                 write the latest state as JSON
                                    (default /tmp/gpuwatch.json for watch)
  --on-event COMMAND                run with `sh -c` once per event
  --json                            check: print the full report
  --quiet                           no stderr lines except errors

Environment variables set the defaults for the flags: GPUWATCH_SENSITIVITY,
GPUWATCH_WINDOW, GPUWATCH_REDUCTION, GPUWATCH_INTERVAL, GPUWATCH_REPORT_URL,
GPUWATCH_STATE_FILE, GPUWATCH_ON_EVENT, GPUWATCH_QUIET=1. GPUWATCH_DISABLED=1
makes `watch` exit at once.

Presets (window, thermal fraction, clock reduction while throttled):
  low     10 min  50%  50%   half speed or worse, a broken heat path
  medium  10 min  50%  30%   the rule Modal's worker logs as a warning
  high     5 min  30%  15%   a 15% loss, still above normal warm-GPU behaviour
";

struct Args {
    command: String,
    seconds: f64,
    thresholds: Thresholds,
    interval: f64,
    report_url: Option<String>,
    state_file: Option<String>,
    on_event: Option<String>,
    json: bool,
    quiet: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut argv = std::env::args().skip(1).peekable();
    let command = match argv.peek() {
        Some(a)
            if a.starts_with('-')
                && a != "--help"
                && a != "-h"
                && a != "--version"
                && a != "-V" =>
        {
            "watch".to_string()
        }
        Some(_) => argv.next().unwrap_or_default(),
        None => "watch".to_string(),
    };
    if command == "--version" || command == "-V" {
        println!("gpuwatch {VERSION}");
        std::process::exit(0);
    }
    if command == "--help" || command == "-h" {
        print!("{USAGE}");
        std::process::exit(0);
    }
    if command == "event" {
        let path = env("GPUWATCH_STATE_FILE").unwrap_or_else(|| "/tmp/gpuwatch.json".into());
        let state: serde_json::Value = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(serde_json::Value::Null);
        match state.get("event") {
            Some(e) if !e.is_null() => {
                println!("{e}");
                std::process::exit(2)
            }
            _ => std::process::exit(0),
        }
    }
    if command != "check" && command != "watch" {
        return Err(format!("unknown command {command:?}"));
    }
    let env_num = |name: &str| -> Result<Option<f64>, String> {
        env(name)
            .map(|v| v.parse::<f64>().map_err(|e| format!("{name}: {e}")))
            .transpose()
    };
    let mut seconds = 30.0;
    let mut sensitivity = match env("GPUWATCH_SENSITIVITY") {
        Some(v) => Sensitivity::parse(&v).ok_or(format!(
            "GPUWATCH_SENSITIVITY: {v:?} is not low, medium or high"
        ))?,
        None => Sensitivity::Medium,
    };
    let mut window = env_num("GPUWATCH_WINDOW")?;
    let mut reduction = env_num("GPUWATCH_REDUCTION")?;
    let mut interval = env_num("GPUWATCH_INTERVAL")?.unwrap_or(2.0);
    let mut report_url =
        Some(env("GPUWATCH_REPORT_URL").unwrap_or_else(|| DEFAULT_REPORT_URL.to_string()));
    let mut state_file = env("GPUWATCH_STATE_FILE");
    let mut on_event = env("GPUWATCH_ON_EVENT");
    let mut json = false;
    let mut quiet = env("GPUWATCH_QUIET").is_some_and(|v| v != "0");
    let mut rest: Vec<String> = argv.collect();
    rest.reverse();
    let value = |flag: &str, rest: &mut Vec<String>| {
        rest.pop().ok_or_else(|| format!("{flag} needs a value"))
    };
    while let Some(flag) = rest.pop() {
        match flag.as_str() {
            "--seconds" => {
                seconds = value(&flag, &mut rest)?
                    .parse()
                    .map_err(|e| format!("--seconds: {e}"))?
            }
            "--sensitivity" => {
                let v = value(&flag, &mut rest)?;
                sensitivity = Sensitivity::parse(&v)
                    .ok_or(format!("--sensitivity: {v:?} is not low, medium or high"))?;
            }
            "--window" => {
                window = Some(
                    value(&flag, &mut rest)?
                        .parse()
                        .map_err(|e| format!("--window: {e}"))?,
                )
            }
            "--reduction" => {
                reduction = Some(
                    value(&flag, &mut rest)?
                        .parse()
                        .map_err(|e| format!("--reduction: {e}"))?,
                )
            }
            "--interval" => {
                interval = value(&flag, &mut rest)?
                    .parse()
                    .map_err(|e| format!("--interval: {e}"))?
            }
            "--report-url" => report_url = Some(value(&flag, &mut rest)?),
            "--state-file" => state_file = Some(value(&flag, &mut rest)?),
            "--on-event" => on_event = Some(value(&flag, &mut rest)?),
            "--json" => json = true,
            "--quiet" => quiet = true,
            _ => return Err(format!("unknown flag {flag:?}")),
        }
    }
    if !(0.2..=60.0).contains(&interval) {
        return Err("--interval must be between 0.2 and 60 seconds".into());
    }
    let mut thresholds = Thresholds::preset(sensitivity);
    if let Some(w) = window {
        if !(0.1..=120.0).contains(&w) {
            return Err("--window must be between 0.1 and 120 minutes".into());
        }
        thresholds.window_minutes = w;
    }
    if let Some(r) = reduction {
        if !(0.0..=1.0).contains(&r) {
            return Err("--reduction must be between 0 and 1".into());
        }
        thresholds.min_reduction = r;
    }
    if command == "watch" && state_file.is_none() {
        state_file = Some("/tmp/gpuwatch.json".into());
    }
    if report_url
        .as_deref()
        .is_some_and(|u| u.is_empty() || u == "none")
    {
        report_url = None;
    }
    Ok(Args {
        command,
        seconds,
        thresholds,
        interval,
        report_url,
        state_file,
        on_event,
        json,
        quiet,
    })
}

/// Where the report comes from, read from the Modal container environment.
/// Token claims are decoded without verification; the collector verifies the
/// token itself from the Authorization header.
#[derive(Clone, Serialize)]
struct Context {
    task_id: Option<String>,
    sandbox_id: Option<String>,
    cloud: Option<String>,
    region: Option<String>,
    environment: Option<String>,
    image_id: Option<String>,
    hostname: Option<String>,
    claims: BTreeMap<String, String>,
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc: u32 = 0;
    let mut bits = 0;
    for c in s.bytes() {
        if c == b'=' {
            break;
        }
        let v = ALPHABET.iter().position(|&a| a == c)? as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

fn token_claims(token: &str) -> BTreeMap<String, String> {
    let mut claims = BTreeMap::new();
    let Some(payload) = token.split('.').nth(1) else {
        return claims;
    };
    let Some(bytes) = base64url_decode(payload) else {
        return claims;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return claims;
    };
    for key in [
        "workspace_id",
        "environment_name",
        "app_id",
        "app_name",
        "function_id",
        "function_name",
        "sandbox_id",
        "sandbox_name",
        "container_id",
        "exp",
    ] {
        if let Some(v) = value.get(key) {
            let s = match v {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            claims.insert(key.to_string(), s);
        }
    }
    claims
}

impl Context {
    fn read() -> Self {
        Self {
            task_id: env("MODAL_TASK_ID"),
            sandbox_id: env("MODAL_SANDBOX_ID"),
            cloud: env("MODAL_CLOUD_PROVIDER")
                .map(|c| c.trim_start_matches("CLOUD_PROVIDER_").to_lowercase()),
            region: env("MODAL_REGION"),
            environment: env("MODAL_ENVIRONMENT"),
            image_id: env("MODAL_IMAGE_ID"),
            hostname: std::fs::read_to_string("/etc/hostname")
                .ok()
                .map(|h| h.trim().to_string()),
            claims: env("MODAL_IDENTITY_TOKEN")
                .map(|t| token_claims(&t))
                .unwrap_or_default(),
        }
    }
}

#[derive(Clone, Serialize)]
struct GpuInfo {
    index: u32,
    name: String,
    uuid: String,
    serial: Option<String>,
    pci_bus_id: Option<String>,
    max_sm_clock_mhz: u32,
    power_limit_w: f64,
    driver: String,
}

#[derive(Serialize)]
struct Report<'a> {
    gpuwatch: &'static str,
    kind: &'static str,
    ts_unix: f64,
    container: &'a Context,
    gpu: &'a GpuInfo,
    window: &'a WindowStats,
    verdict: &'a Verdict,
}

/// The daemon's state file: the newest stats per GPU and the last event.
#[derive(Serialize)]
struct State<'a> {
    gpuwatch: &'static str,
    updated_unix: f64,
    event: Option<&'a serde_json::Value>,
    gpus: Vec<GpuState<'a>>,
}

#[derive(Serialize)]
struct GpuState<'a> {
    gpu: &'a GpuInfo,
    window: Option<WindowStats>,
    verdict: Option<Verdict>,
}

fn now_unix() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

struct Gpu<'n> {
    info: GpuInfo,
    device: nvml_wrapper::Device<'n>,
    window: Window,
}

/// Loads NVML and opens every GPU. The library handle is leaked on purpose:
/// device handles borrow it for the life of the process, and a reopen after a
/// snapshot restore happens a handful of times at most.
fn open_session(args: &Args) -> Result<Vec<Gpu<'static>>, String> {
    let nvml = nvml_wrapper::Nvml::builder()
        .lib_path(OsStr::new("libnvidia-ml.so.1"))
        .init()
        .map_err(|e| format!("NVML init failed ({e}); is this a GPU container?"))?;
    open_gpus(Box::leak(Box::new(nvml)), args)
}

fn open_gpus<'n>(nvml: &'n nvml_wrapper::Nvml, args: &Args) -> Result<Vec<Gpu<'n>>, String> {
    let driver = nvml
        .sys_driver_version()
        .unwrap_or_else(|_| "unknown".into());
    let count = nvml
        .device_count()
        .map_err(|e| format!("NVML device count: {e}"))?;
    let mut gpus = Vec::new();
    for index in 0..count {
        let device = nvml
            .device_by_index(index)
            .map_err(|e| format!("GPU {index}: {e}"))?;
        let max_sm_clock_mhz = device
            .max_clock_info(nvml_wrapper::enum_wrappers::device::Clock::SM)
            .map_err(|e| format!("GPU {index} max SM clock: {e}"))?;
        let info = GpuInfo {
            index,
            name: device.name().unwrap_or_else(|_| "unknown".into()),
            uuid: device
                .uuid()
                .map_err(|e| format!("GPU {index} uuid: {e}"))?,
            serial: device.serial().ok(),
            pci_bus_id: device.pci_info().ok().map(|p| p.bus_id),
            max_sm_clock_mhz,
            power_limit_w: device
                .enforced_power_limit()
                .map(|mw| f64::from(mw) / 1000.0)
                .unwrap_or(0.0),
            driver: driver.clone(),
        };
        let window = Window::new(args.thresholds.window_samples(args.interval), args.interval);
        gpus.push(Gpu {
            info,
            device,
            window,
        });
    }
    if gpus.is_empty() {
        return Err("no GPU visible to NVML".into());
    }
    Ok(gpus)
}

fn sample(gpu: &Gpu) -> Result<Sample, String> {
    use nvml_wrapper::enum_wrappers::device::{Clock, TemperatureSensor};
    let d = &gpu.device;
    let err = |what: &str, e: nvml_wrapper::error::NvmlError| {
        format!("GPU {} {what}: {e}", gpu.info.index)
    };
    Ok(Sample {
        util_pct: d
            .utilization_rates()
            .map_err(|e| err("utilization", e))?
            .gpu,
        sm_clock_mhz: d.clock_info(Clock::SM).map_err(|e| err("SM clock", e))?,
        max_sm_clock_mhz: gpu.info.max_sm_clock_mhz,
        temp_c: d
            .temperature(TemperatureSensor::Gpu)
            .map_err(|e| err("temperature", e))?,
        power_w: d
            .power_usage()
            .map(|mw| f64::from(mw) / 1000.0)
            .unwrap_or(0.0),
        power_limit_w: gpu.info.power_limit_w,
        reasons: d
            .current_throttle_reasons()
            .map_err(|e| err("clock event reasons", e))?
            .bits(),
    })
}

fn post_report(url: &str, body: &str, quiet: bool) {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .build()
        .into();
    let mut req = agent.post(url).header("Content-Type", "application/json");
    if let Some(token) = env("MODAL_IDENTITY_TOKEN") {
        req = req.header("Authorization", &format!("Bearer {token}"));
    }
    match req.send(body) {
        Ok(resp) if resp.status().is_success() => {
            if !quiet {
                eprintln!("gpuwatch: reported to {url}");
            }
        }
        Ok(resp) => eprintln!("gpuwatch: report to {url} failed: HTTP {}", resp.status()),
        Err(e) => eprintln!("gpuwatch: report to {url} failed: {e}"),
    }
}

fn write_state(path: &str, state: &State) {
    let tmp = format!("{path}.tmp");
    let body = match serde_json::to_vec_pretty(state) {
        Ok(b) => b,
        Err(e) => return eprintln!("gpuwatch: state: {e}"),
    };
    if let Err(e) = std::fs::write(&tmp, body).and_then(|_| std::fs::rename(&tmp, path)) {
        eprintln!("gpuwatch: write {path}: {e}");
    }
}

fn run_on_event(command: &str, report_json: &str, report: &Report) {
    let status = std::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .env("GPUWATCH_REPORT", report_json)
        .env("GPUWATCH_GPU_UUID", &report.gpu.uuid)
        .env("GPUWATCH_GPU_INDEX", report.gpu.index.to_string())
        .env(
            "GPUWATCH_CLOCK_RATIO",
            format!("{:.3}", report.window.clock_ratio_busy),
        )
        .status();
    match status {
        Ok(s) if s.success() => {}
        Ok(s) => eprintln!("gpuwatch: --on-event exited with {s}"),
        Err(e) => eprintln!("gpuwatch: --on-event: {e}"),
    }
}

fn describe(report: &Report) -> String {
    let w = report.window;
    format!(
        "GPU {} {} ({}): {} busy min, thermal {:.0}% of samples, clock {:.0}% of max{}, temp max {} C, power {:.2} of limit",
        report.gpu.index,
        report.gpu.name,
        report.gpu.uuid,
        format_args!("{:.1}", w.busy_minutes),
        w.thermal_fraction * 100.0,
        w.clock_ratio_busy * 100.0,
        w.clock_ratio_throttled.map(|r| format!(" ({:.0}% while throttled)", r * 100.0)).unwrap_or_default(),
        w.temp_c_max,
        w.power_ratio_mean,
    )
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("gpuwatch: {e}\n\n{USAGE}");
            return ExitCode::from(1);
        }
    };
    if args.command == "watch" && env("GPUWATCH_DISABLED").is_some_and(|v| v != "0") {
        return ExitCode::from(0);
    }
    // Held for the life of the process; a second daemon in the same container
    // (every Python interpreter tries to start one) exits here, silently.
    let _lock = if args.command == "watch" {
        match std::fs::File::create(LOCK_FILE)
            .and_then(|f| f.try_lock().map(|_| f).map_err(std::io::Error::other))
        {
            Ok(f) => Some(f),
            Err(_) => return ExitCode::from(0),
        }
    } else {
        None
    };
    let mut gpus = match open_session(&args) {
        Ok(g) => g,
        Err(e) => {
            eprintln!("gpuwatch: {e}");
            return ExitCode::from(1);
        }
    };
    let context = Context::read();
    let interval = Duration::from_secs_f64(args.interval);
    if !args.quiet {
        eprintln!(
            "gpuwatch {VERSION}: {} GPU(s), {} sensitivity, window {} busy min, fires at {:.0}% thermal samples and {:.0}% clock reduction, {}s interval",
            gpus.len(),
            serde_json::to_value(args.thresholds.sensitivity).unwrap_or_default().as_str().unwrap_or("?"),
            args.thresholds.window_minutes,
            args.thresholds.min_thermal_fraction * 100.0,
            args.thresholds.min_reduction * 100.0,
            args.interval
        );
    }

    if args.command == "check" {
        let deadline = Instant::now() + Duration::from_secs_f64(args.seconds);
        while Instant::now() < deadline {
            for gpu in &mut gpus {
                match sample(gpu) {
                    Ok(s) => gpu.window.push(s),
                    Err(e) => eprintln!("gpuwatch: {e}"),
                }
            }
            std::thread::sleep(interval);
        }
        let mut throttling = false;
        for gpu in &gpus {
            let Some((window, verdict)) = gpu.window.evaluate(&args.thresholds, false) else {
                println!(
                    "GPU {} {} ({}): idle during the check, no verdict",
                    gpu.info.index, gpu.info.name, gpu.info.uuid
                );
                continue;
            };
            let report = Report {
                gpuwatch: VERSION,
                kind: "check",
                ts_unix: now_unix(),
                container: &context,
                gpu: &gpu.info,
                window: &window,
                verdict: &verdict,
            };
            let json = serde_json::to_string(&report).unwrap_or_default();
            if args.json {
                println!("{json}");
            } else {
                println!(
                    "{}{}",
                    if verdict.event { "THROTTLING " } else { "ok " },
                    describe(&report)
                );
            }
            if verdict.event {
                throttling = true;
                if let Some(url) = &args.report_url {
                    post_report(url, &json, args.quiet);
                }
            }
        }
        return ExitCode::from(if throttling { 2 } else { 0 });
    }

    // watch
    let mut last_event: Option<serde_json::Value> = None;
    let mut last_state_write = Instant::now();
    let mut failures = 0u32;
    loop {
        // A restored memory snapshot lands on another GPU: after repeated NVML
        // failures the library and the device handles are opened again. The
        // windows start over, which is right for a different device.
        if failures >= 5 {
            match open_session(&args) {
                Ok(g) => {
                    gpus = g;
                    failures = 0;
                    if !args.quiet {
                        eprintln!("gpuwatch: NVML reopened, {} GPU(s)", gpus.len());
                    }
                }
                Err(e) => {
                    eprintln!("gpuwatch: {e}");
                    std::thread::sleep(Duration::from_secs(10));
                    continue;
                }
            }
        }
        let mut fired = false;
        let mut errors = 0u32;
        for gpu in &mut gpus {
            match sample(gpu) {
                Ok(s) => gpu.window.push(s),
                Err(e) => {
                    eprintln!("gpuwatch: {e}");
                    errors += 1;
                    continue;
                }
            }
            let Some((window, verdict)) = gpu.window.evaluate(&args.thresholds, true) else {
                continue;
            };
            if !verdict.event {
                continue;
            }
            let report = Report {
                gpuwatch: VERSION,
                kind: "event",
                ts_unix: now_unix(),
                container: &context,
                gpu: &gpu.info,
                window: &window,
                verdict: &verdict,
            };
            let json = serde_json::to_string(&report).unwrap_or_default();
            eprintln!("gpuwatch: THROTTLING {}", describe(&report));
            if let Some(url) = &args.report_url {
                post_report(url, &json, args.quiet);
            }
            last_event = serde_json::from_str(&json).ok();
            fired = true;
            if let Some(cmd) = &args.on_event {
                run_on_event(cmd, &json, &report);
            }
            // The next event needs a fresh full window.
            gpu.window.clear();
        }
        failures = if errors > 0 { failures + 1 } else { 0 };
        if fired || last_state_write.elapsed() >= Duration::from_secs(30) {
            if let Some(path) = &args.state_file {
                let state = State {
                    gpuwatch: VERSION,
                    updated_unix: now_unix(),
                    event: last_event.as_ref(),
                    gpus: gpus
                        .iter()
                        .map(|g| {
                            let (window, verdict) = match g.window.evaluate(&args.thresholds, false)
                            {
                                Some((w, v)) => (Some(w), Some(v)),
                                None => (None, None),
                            };
                            GpuState {
                                gpu: &g.info,
                                window,
                                verdict,
                            }
                        })
                        .collect(),
                };
                write_state(path, &state);
            }
            last_state_write = Instant::now();
        }
        std::thread::sleep(interval);
    }
}
