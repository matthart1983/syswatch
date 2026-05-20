//! Per-process network bandwidth attribution.
//!
//! Wraps `netwatch_sdk::collectors::process_bandwidth::attribute`, which
//! splits the host's interface throughput proportionally to each
//! process's ESTABLISHED connection count. The kernel doesn't expose
//! true per-PID byte accounting cheaply on either macOS or Linux, so
//! this is an approximation — but it's the same shape netwatch's TUI
//! uses, and good enough to answer "which process is eating the wire."
//!
//! Costs: `lsof` (macOS) and `ss` (Linux) take 50–500 ms on a busy host.
//! We cache the per-PID map for `REFRESH` and re-sample no more often
//! than that, so the procs tab stays smooth even at sub-second tick
//! rates.
//!
//! We parse the platform commands locally instead of calling the SDK's full
//! `collect_connections()`: macOS enriches RTT via `nettop`, but syswatch's
//! per-process bandwidth attribution does not use RTT and should not wait on it.

use std::collections::HashMap;
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use netwatch_sdk::collectors::connections::ConnectionDetail;
use netwatch_sdk::collectors::process_bandwidth::attribute;
use netwatch_sdk::types::InterfaceMetric;

use super::model::InterfaceTick;

const REFRESH: Duration = Duration::from_secs(2);
const COMMAND_TIMEOUT: Duration = Duration::from_millis(1500);
/// Top-N cap matches the "top X procs" intuition without unbounded
/// growth on hosts with thousands of connections.
const MAX_PROCS: usize = 256;

pub struct ProcessBandwidthCollector {
    last_request_at: Option<Instant>,
    cached: HashMap<u32, (f64, f64)>, // pid -> (rx_rate, tx_rate)
    in_flight: bool,
    request_tx: Option<Sender<Vec<InterfaceTick>>>,
    result_rx: Receiver<HashMap<u32, (f64, f64)>>,
}

impl ProcessBandwidthCollector {
    pub fn new() -> Self {
        let (request_tx, request_rx) = mpsc::channel();
        let (result_tx, result_rx) = mpsc::channel();
        let request_tx = std::thread::Builder::new()
            .name("syswatch-proc-bandwidth".into())
            .spawn(move || bandwidth_worker(request_rx, result_tx))
            .ok()
            .map(|_| request_tx);
        Self {
            // Avoid paying the lsof/ss subprocess cost on the first frame.
            // Per-PID bandwidth is a refinement; the dashboard should render
            // even if connection enumeration is slow on a busy host.
            last_request_at: Some(Instant::now()),
            cached: HashMap::new(),
            in_flight: false,
            request_tx,
            result_rx,
        }
    }

    /// Returns the per-PID rate map. Re-collects at most every REFRESH;
    /// otherwise returns the cached map. `current_net` is whatever the
    /// outer Collector just gathered for interfaces — we re-shape it
    /// into the SDK's `InterfaceMetric` so the attribution function can
    /// allocate by interface throughput.
    pub fn sample(&mut self, current_net: &[InterfaceTick]) -> HashMap<u32, (f64, f64)> {
        while let Ok(result) = self.result_rx.try_recv() {
            self.cached = result;
            self.in_flight = false;
        }

        let stale = self
            .last_request_at
            .map(|t| t.elapsed() >= REFRESH)
            .unwrap_or(true);
        if stale && !self.in_flight {
            if let Some(tx) = self.request_tx.as_ref() {
                match tx.send(current_net.to_vec()) {
                    Ok(()) => {
                        self.in_flight = true;
                        self.last_request_at = Some(Instant::now());
                    }
                    Err(_) => {
                        self.request_tx = None;
                        self.in_flight = false;
                    }
                }
            }
        }
        self.cached.clone()
    }
}

fn bandwidth_worker(
    request_rx: Receiver<Vec<InterfaceTick>>,
    result_tx: Sender<HashMap<u32, (f64, f64)>>,
) {
    while let Ok(current_net) = request_rx.recv() {
        if result_tx.send(compute(&current_net)).is_err() {
            break;
        }
    }
}

fn compute(current_net: &[InterfaceTick]) -> HashMap<u32, (f64, f64)> {
    let conns = collect_process_connections();
    if conns.is_empty() {
        return HashMap::new();
    }
    let metrics: Vec<InterfaceMetric> = current_net
        .iter()
        .map(|i| InterfaceMetric {
            name: i.name.clone(),
            is_up: i.is_up,
            rx_bytes: i.rx_bytes,
            tx_bytes: i.tx_bytes,
            rx_bytes_delta: 0,
            tx_bytes_delta: 0,
            rx_packets: 0,
            tx_packets: 0,
            rx_errors: 0,
            tx_errors: 0,
            rx_drops: 0,
            tx_drops: 0,
            rx_rate: Some(i.rx_rate),
            tx_rate: Some(i.tx_rate),
            rx_history: None,
            tx_history: None,
        })
        .collect();
    let attributed = attribute(&conns, &metrics, MAX_PROCS);
    attributed
        .into_iter()
        .filter_map(|p| p.pid.map(|pid| (pid, (p.rx_rate, p.tx_rate))))
        .collect()
}

#[cfg(target_os = "linux")]
fn collect_process_connections() -> Vec<ConnectionDetail> {
    let text = run_command_with_timeout("ss", &["-tunapi"], COMMAND_TIMEOUT).unwrap_or_default();
    parse_ss_connections(&text)
}

#[cfg(target_os = "macos")]
fn collect_process_connections() -> Vec<ConnectionDetail> {
    let text =
        run_command_with_timeout("lsof", &["-i", "-n", "-P", "-F", "pcPtTn"], COMMAND_TIMEOUT)
            .unwrap_or_default();
    parse_macos_lsof_connections(&text)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn collect_process_connections() -> Vec<ConnectionDetail> {
    Vec::new()
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn run_command_with_timeout(program: &str, args: &[&str], timeout: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;

    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = stdout.read_to_string(&mut text);
        text
    });

    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let _ = child.wait();
                return reader.join().ok();
            }
            Ok(None) if started.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return None;
            }
        }
    }
}

#[cfg(any(target_os = "linux", test))]
fn parse_ss_connections(text: &str) -> Vec<ConnectionDetail> {
    let mut connections: Vec<ConnectionDetail> = Vec::new();

    for line in text.lines().skip(1) {
        if line.starts_with(|c: char| c.is_whitespace()) {
            if let Some(rtt_us) = parse_ss_rtt_us(line) {
                if let Some(last) = connections.last_mut() {
                    last.kernel_rtt_us = Some(rtt_us);
                }
            }
            continue;
        }

        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 6 {
            continue;
        }

        let protocol = cols[0].to_uppercase();
        let state = match cols[1] {
            "ESTAB" => "ESTABLISHED".to_string(),
            other => other.to_string(),
        };
        let local_addr = cols[4].to_string();
        let remote_addr = cols[5].to_string();
        let (pid, process_name) = cols
            .get(6)
            .map(|field| parse_ss_process(field))
            .unwrap_or((None, None));

        connections.push(ConnectionDetail {
            protocol,
            local_addr,
            remote_addr,
            state,
            pid,
            process_name,
            kernel_rtt_us: None,
        });
    }

    connections
}

#[cfg(any(target_os = "linux", test))]
fn parse_ss_rtt_us(line: &str) -> Option<f64> {
    for token in line.split_whitespace() {
        if let Some(rest) = token.strip_prefix("rtt:") {
            let srtt_ms: f64 = rest.split('/').next()?.parse().ok()?;
            return Some(srtt_ms * 1000.0);
        }
    }
    None
}

#[cfg(any(target_os = "linux", test))]
fn parse_ss_process(field: &str) -> (Option<u32>, Option<String>) {
    let name = field.split('"').nth(1).map(|s| s.to_string());
    let pid = field
        .split("pid=")
        .nth(1)
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.parse().ok());
    (pid, name)
}

#[cfg(target_os = "macos")]
fn parse_macos_lsof_connections(text: &str) -> Vec<ConnectionDetail> {
    let mut connections = Vec::new();

    let mut pid: Option<u32> = None;
    let mut process_name: Option<String> = None;
    let mut protocol = String::new();
    let mut state = String::new();
    let mut local_addr = String::new();
    let mut remote_addr = String::new();
    let mut has_network = false;

    for line in text.lines().filter(|line| !line.is_empty()) {
        let tag = line.as_bytes()[0];
        let value = &line[1..];

        match tag {
            b'p' => {
                flush_connection(
                    &mut connections,
                    &mut has_network,
                    &protocol,
                    &local_addr,
                    &remote_addr,
                    &state,
                    pid,
                    &process_name,
                );
                pid = value.parse().ok();
                process_name = None;
            }
            b'c' => {
                process_name = Some(value.to_string());
            }
            b'f' => {
                flush_connection(
                    &mut connections,
                    &mut has_network,
                    &protocol,
                    &local_addr,
                    &remote_addr,
                    &state,
                    pid,
                    &process_name,
                );
                protocol.clear();
                state.clear();
            }
            b'P' => {
                protocol = value.to_string();
            }
            b'T' => {
                if let Some(st) = value.strip_prefix("ST=") {
                    state = st.to_string();
                }
            }
            b'n' => {
                if let Some(arrow_pos) = value.find("->") {
                    local_addr = value[..arrow_pos]
                        .trim_matches(|c| c == '[' || c == ']')
                        .to_string();
                    remote_addr = value[arrow_pos + 2..]
                        .trim_matches(|c| c == '[' || c == ']')
                        .to_string();
                } else {
                    local_addr = value.to_string();
                    remote_addr = "*:*".to_string();
                };
                has_network = true;
            }
            _ => {}
        }
    }

    flush_connection(
        &mut connections,
        &mut has_network,
        &protocol,
        &local_addr,
        &remote_addr,
        &state,
        pid,
        &process_name,
    );

    connections
}

#[cfg(target_os = "macos")]
fn flush_connection(
    connections: &mut Vec<ConnectionDetail>,
    has_network: &mut bool,
    protocol: &str,
    local_addr: &str,
    remote_addr: &str,
    state: &str,
    pid: Option<u32>,
    process_name: &Option<String>,
) {
    if !*has_network {
        return;
    }
    connections.push(ConnectionDetail {
        protocol: protocol.to_string(),
        local_addr: local_addr.to_string(),
        remote_addr: remote_addr.to_string(),
        state: state.to_string(),
        pid,
        process_name: process_name.clone(),
        kernel_rtt_us: None,
    });
    *has_network = false;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_interfaces_yields_zero_rates() {
        // No interfaces means the SDK has no throughput to allocate;
        // any PIDs returned (real ESTABLISHED conns on the test host)
        // must therefore have zero rates. We don't assert empty
        // because the host running tests typically has active sockets.
        let map = compute(&[]);
        for (_pid, (rx, tx)) in &map {
            assert_eq!(*rx, 0.0);
            assert_eq!(*tx, 0.0);
        }
    }

    #[test]
    fn cache_short_circuits_within_refresh_window() {
        let mut c = ProcessBandwidthCollector::new();
        // First call populates cached + last_at; subsequent calls
        // within REFRESH should not re-invoke compute (we can't easily
        // verify the no-call directly, but we can verify the timestamp
        // doesn't move and the cache shape is stable).
        let _ = c.sample(&[]);
        let first_at = c.last_request_at;
        let _ = c.sample(&[]);
        assert_eq!(c.last_request_at, first_at);
    }

    #[test]
    fn ss_parser_extracts_pid_state_and_rtt() {
        let text = "\
Netid State  Recv-Q Send-Q Local Address:Port Peer Address:Port Process
tcp   ESTAB  0      0      127.0.0.1:55555 93.184.216.34:443 users:((\"curl\",pid=1234,fd=7))
         cubic wscale:7,7 rto:204 rtt:12.5/1.2 ato:40 mss:1448
";

        let conns = parse_ss_connections(text);
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].protocol, "TCP");
        assert_eq!(conns[0].state, "ESTABLISHED");
        assert_eq!(conns[0].local_addr, "127.0.0.1:55555");
        assert_eq!(conns[0].remote_addr, "93.184.216.34:443");
        assert_eq!(conns[0].pid, Some(1234));
        assert_eq!(conns[0].process_name.as_deref(), Some("curl"));
        assert_eq!(conns[0].kernel_rtt_us, Some(12_500.0));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_lsof_parser_extracts_pid_and_established_state() {
        let text = "\
p123
cSafari
f42
Ptcp
TST=ESTABLISHED
n192.168.1.10:55555->17.253.144.10:443
";

        let conns = parse_macos_lsof_connections(text);
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].pid, Some(123));
        assert_eq!(conns[0].process_name.as_deref(), Some("Safari"));
        assert_eq!(conns[0].protocol, "tcp");
        assert_eq!(conns[0].state, "ESTABLISHED");
        assert_eq!(conns[0].local_addr, "192.168.1.10:55555");
        assert_eq!(conns[0].remote_addr, "17.253.144.10:443");
        assert_eq!(conns[0].kernel_rtt_us, None);
    }
}
