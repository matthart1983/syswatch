//! Per-process network bandwidth — measured where the platform allows,
//! attributed where it doesn't.
//!
//! **macOS**: `nettop -P` reports true per-process byte counters from
//! the kernel's ntstat (no sudo). The worker keeps the previous
//! cumulative sample and deltas into rates — these are measurements,
//! not estimates, and the UI shows them unmarked.
//!
//! **Linux / fallback**: true per-PID byte accounting needs eBPF and
//! privileges, so we split the host's non-loopback interface throughput
//! proportionally to each process's connection count (TCP ESTABLISHED
//! plus connected UDP, so QUIC-heavy processes aren't invisible).
//! Every connection is weighted equally — an idle SSH session counts
//! the same as a busy stream — so these are estimates and the UI
//! prefixes the column headers with `~`.
//!
//! Costs: `nettop` / `lsof` (macOS) and `ss` (Linux) take 50–500 ms on
//! a busy host. We cache the per-PID map for `REFRESH` and re-sample no
//! more often than that, so the procs tab stays smooth even at
//! sub-second tick rates.

use std::collections::HashMap;
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use netwatch_sdk::collectors::connections::ConnectionDetail;

use super::command::run_with_timeout;
use super::model::InterfaceTick;

const REFRESH: Duration = Duration::from_secs(2);
const COMMAND_TIMEOUT: Duration = Duration::from_millis(1500);
/// Top-N cap matches the "top X procs" intuition without unbounded
/// growth on hosts with thousands of connections.
const MAX_PROCS: usize = 256;

/// Per-PID (rx_rate, tx_rate) map plus the estimated-vs-measured flag.
type BandwidthResult = (HashMap<u32, (f64, f64)>, bool);

pub struct ProcessBandwidthCollector {
    last_request_at: Option<Instant>,
    cached: HashMap<u32, (f64, f64)>, // pid -> (rx_rate, tx_rate)
    /// Whether `cached` came from the connection-count approximation
    /// (true) or real per-PID counters (false). Drives the `~` marker
    /// on the NET column headers.
    cached_estimated: bool,
    in_flight: bool,
    // Bounded request channel (capacity 1). The `in_flight` guard
    // already serializes requests, so this should never block or hit
    // `Full` in normal operation — but if a future change drops the
    // guard, the bounded channel turns a silent OOM into a visible
    // dropped tick instead.
    request_tx: Option<SyncSender<Vec<InterfaceTick>>>,
    result_rx: Receiver<BandwidthResult>,
}

impl ProcessBandwidthCollector {
    pub fn new() -> Self {
        let (request_tx, request_rx) = mpsc::sync_channel(1);
        let (result_tx, result_rx) = mpsc::channel();
        let request_tx = std::thread::Builder::new()
            .name("syswatch-proc-bandwidth".into())
            .spawn(move || run_bandwidth_worker_with_recovery(request_rx, result_tx))
            .ok()
            .map(|_| request_tx);
        Self {
            // Avoid paying the nettop/lsof/ss subprocess cost on the first
            // frame. Per-PID bandwidth is a refinement; the dashboard should
            // render even if connection enumeration is slow on a busy host.
            last_request_at: Some(Instant::now()),
            cached: HashMap::new(),
            cached_estimated: true,
            in_flight: false,
            request_tx,
            result_rx,
        }
    }

    /// Returns the per-PID rate map plus whether it's an estimate
    /// (connection-count attribution) or measured (nettop). Re-collects
    /// at most every REFRESH; otherwise returns the cached map.
    /// `current_net` is whatever the outer Collector just gathered for
    /// interfaces — the approximation allocates its throughput.
    pub fn sample(&mut self, current_net: &[InterfaceTick]) -> BandwidthResult {
        while let Ok((result, estimated)) = self.result_rx.try_recv() {
            self.cached = result;
            self.cached_estimated = estimated;
            self.in_flight = false;
        }

        let stale = self
            .last_request_at
            .map(|t| t.elapsed() >= REFRESH)
            .unwrap_or(true);
        if stale && !self.in_flight {
            if let Some(tx) = self.request_tx.as_ref() {
                match tx.try_send(current_net.to_vec()) {
                    Ok(()) => {
                        self.in_flight = true;
                        self.last_request_at = Some(Instant::now());
                    }
                    Err(TrySendError::Full(_)) => {
                        // Should be unreachable thanks to `in_flight`,
                        // but if the guard ever drifts out of sync with
                        // the channel state, prefer a dropped tick over
                        // an unbounded queue.
                    }
                    Err(TrySendError::Disconnected(_)) => {
                        self.request_tx = None;
                        self.in_flight = false;
                    }
                }
            }
        }
        (self.cached.clone(), self.cached_estimated)
    }
}

fn bandwidth_worker(
    request_rx: &Receiver<Vec<InterfaceTick>>,
    result_tx: &std::sync::mpsc::Sender<BandwidthResult>,
) -> WorkerOutcome {
    // nettop counters are cumulative, so the measured path is stateful:
    // the state lives for the worker's lifetime and resets (one blank
    // interval) if the worker is restarted after a panic.
    #[cfg(target_os = "macos")]
    let mut nettop = NettopState::default();
    while let Ok(current_net) = request_rx.recv() {
        #[cfg(target_os = "macos")]
        let result = match nettop.sample() {
            Some(measured) => (measured, false),
            None => (compute_approx(&current_net), true),
        };
        #[cfg(not(target_os = "macos"))]
        let result = (compute_approx(&current_net), true);
        if result_tx.send(result).is_err() {
            return WorkerOutcome::ChannelDisconnected;
        }
    }
    WorkerOutcome::ChannelDisconnected
}

/// Why the bandwidth worker stopped iterating. Used to decide whether
/// to retry (after a caught panic) or exit cleanly (channel closed).
enum WorkerOutcome {
    ChannelDisconnected,
}

const WORKER_BACKOFF_INITIAL: Duration = Duration::from_secs(1);
const WORKER_BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Wrap `bandwidth_worker` in `catch_unwind` + exponential backoff so a
/// panic inside the lsof / ss parser, or inside the SDK's attribution
/// math, doesn't permanently silence per-process bandwidth. Clean
/// channel disconnect exits without retrying.
fn run_bandwidth_worker_with_recovery(
    request_rx: Receiver<Vec<InterfaceTick>>,
    result_tx: std::sync::mpsc::Sender<BandwidthResult>,
) {
    let mut backoff = WORKER_BACKOFF_INITIAL;
    loop {
        let request_rx_ref = &request_rx;
        let result_tx_ref = &result_tx;
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bandwidth_worker(request_rx_ref, result_tx_ref)
        }));
        match result {
            Ok(WorkerOutcome::ChannelDisconnected) => return,
            Err(payload) => {
                let msg = if let Some(s) = payload.downcast_ref::<&'static str>() {
                    s.to_string()
                } else if let Some(s) = payload.downcast_ref::<String>() {
                    s.clone()
                } else {
                    "non-string panic payload".to_string()
                };
                eprintln!(
                    "syswatch-proc-bandwidth: worker panicked ({msg}); restarting in {:?}",
                    backoff
                );
                std::thread::sleep(backoff);
                backoff = (backoff * 2).min(WORKER_BACKOFF_MAX);
            }
        }
    }
}

fn compute_approx(current_net: &[InterfaceTick]) -> HashMap<u32, (f64, f64)> {
    approx_from(&collect_process_connections(), current_net)
}

/// Split non-loopback interface throughput across processes by their
/// share of attributable connections. Pure so tests can drive it with
/// synthetic connections + interfaces.
fn approx_from(
    conns: &[ConnectionDetail],
    current_net: &[InterfaceTick],
) -> HashMap<u32, (f64, f64)> {
    let mut counts: HashMap<u32, u32> = HashMap::new();
    let mut total: u32 = 0;
    for c in conns {
        let Some(pid) = c.pid else { continue };
        if !counts_toward_attribution(c) {
            continue;
        }
        *counts.entry(pid).or_insert(0) += 1;
        total += 1;
    }
    if total == 0 {
        return HashMap::new();
    }

    // Loopback stays out of the totals: local app↔db traffic shows up
    // as both rx and tx on lo and would otherwise be handed to procs
    // that only hold internet connections.
    let (rx_total, tx_total) = current_net
        .iter()
        .filter(|i| !is_loopback_iface(&i.name))
        .fold((0.0, 0.0), |acc, i| (acc.0 + i.rx_rate, acc.1 + i.tx_rate));

    let mut by_count: Vec<(u32, u32)> = counts.into_iter().collect();
    by_count.sort_by_key(|&(_, c)| std::cmp::Reverse(c));
    by_count.truncate(MAX_PROCS);
    by_count
        .into_iter()
        .map(|(pid, count)| {
            let fraction = count as f64 / total as f64;
            (pid, (rx_total * fraction, tx_total * fraction))
        })
        .collect()
}

/// Which sockets earn a slice of the throughput pool. TCP ESTABLISHED
/// plus connected UDP — QUIC carries a large share of browser traffic
/// and would otherwise be attributed to whoever holds TCP connections.
/// Loopback flows never count: their bytes aren't in the (non-loopback)
/// pool being divided.
fn counts_toward_attribution(c: &ConnectionDetail) -> bool {
    if is_loopback_addr(&c.local_addr) || is_loopback_addr(&c.remote_addr) {
        return false;
    }
    // ss reports connected UDP as ESTAB too; this arm covers TCP on
    // both platforms and UDP on Linux.
    if c.state == "ESTABLISHED" {
        return true;
    }
    // macOS lsof emits UDP flows without a state line; a concrete
    // remote peer is the "connected" signal there.
    c.protocol.eq_ignore_ascii_case("udp") && has_peer(&c.remote_addr)
}

/// Unconnected sockets show a wildcard peer: `*:*` from lsof,
/// `0.0.0.0:*` / `[::]:*` from ss.
fn has_peer(addr: &str) -> bool {
    !addr.is_empty() && !addr.ends_with(":*")
}

fn is_loopback_addr(addr: &str) -> bool {
    // Covers ss ("127.0.0.1:80", "[::1]:80") and the lsof parser's
    // bracket-stripped form ("::1]:50000").
    addr.starts_with("127.") || addr.starts_with("::1") || addr.starts_with("[::1")
}

fn is_loopback_iface(name: &str) -> bool {
    // lo (Linux), lo0/lo1… (BSD/macOS).
    name == "lo"
        || (name.len() > 2
            && name.starts_with("lo")
            && name[2..].chars().all(|c| c.is_ascii_digit()))
}

// ── macOS measured path (nettop) ───────────────────────────────────

/// Stateful nettop sampler. `nettop -P` reports cumulative per-process
/// bytes from the kernel's ntstat — true measurement, no sudo. Rates
/// need two samples, so the first call primes the counters and returns
/// an empty (but still "measured") map.
/// pid → cumulative (bytes_in, bytes_out) as nettop reports them.
#[cfg(any(target_os = "macos", test))]
type NettopCounters = HashMap<u32, (u64, u64)>;

#[cfg(target_os = "macos")]
#[derive(Default)]
struct NettopState {
    prev: Option<(NettopCounters, Instant)>,
}

#[cfg(target_os = "macos")]
impl NettopState {
    /// None = nettop missing/failed/empty → caller falls back to the
    /// connection-count approximation for this round.
    fn sample(&mut self) -> Option<HashMap<u32, (f64, f64)>> {
        // -t external keeps loopback traffic out of the counters so
        // the numbers describe the wire, matching the host-level KPIs.
        let text = run_with_timeout(
            "nettop",
            &[
                "-P",
                "-J",
                "bytes_in,bytes_out",
                "-t",
                "external",
                "-x",
                "-L",
                "1",
            ],
            COMMAND_TIMEOUT,
        )?;
        let cur = parse_nettop(&text);
        if cur.is_empty() {
            return None;
        }
        let now = Instant::now();
        let out = match self.prev.take() {
            Some((prev, at)) => {
                let dt = now.duration_since(at).as_secs_f64().max(0.001);
                cur.iter()
                    .filter_map(|(pid, &(rx, tx))| {
                        // PIDs without a previous sample wait a round;
                        // saturating_sub absorbs pid-reuse counter resets.
                        let &(prx, ptx) = prev.get(pid)?;
                        Some((
                            *pid,
                            (
                                rx.saturating_sub(prx) as f64 / dt,
                                tx.saturating_sub(ptx) as f64 / dt,
                            ),
                        ))
                    })
                    .collect()
            }
            None => HashMap::new(),
        };
        self.prev = Some((cur, now));
        Some(out)
    }
}

/// Parse `nettop -P -J bytes_in,bytes_out -x -L 1` CSV into
/// pid → cumulative (bytes_in, bytes_out). Row shape:
/// `time,name.pid,bytes_in,bytes_out,` — the name is nettop-truncated
/// but the pid after the last dot survives, and a name containing
/// commas just widens the row (byte columns are anchored to the end).
#[cfg(any(target_os = "macos", test))]
fn parse_nettop(text: &str) -> NettopCounters {
    let mut out = NettopCounters::new();
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split(',').collect();
        if fields.len() < 4 {
            continue;
        }
        let n = fields.len();
        // Trailing comma yields an empty last field; tolerate its absence.
        let (in_idx, out_idx) = if fields[n - 1].is_empty() {
            (n - 3, n - 2)
        } else {
            (n - 2, n - 1)
        };
        let Ok(bytes_in) = fields[in_idx].parse::<u64>() else {
            continue;
        };
        let Ok(bytes_out) = fields[out_idx].parse::<u64>() else {
            continue;
        };
        let name_pid = fields[1..in_idx].join(",");
        let Some((_, pid_str)) = name_pid.rsplit_once('.') else {
            continue;
        };
        let Ok(pid) = pid_str.parse::<u32>() else {
            continue;
        };
        // -P emits one row per process; sum defensively if it doesn't.
        let e = out.entry(pid).or_insert((0, 0));
        e.0 = e.0.saturating_add(bytes_in);
        e.1 = e.1.saturating_add(bytes_out);
    }
    out
}

#[cfg(target_os = "linux")]
fn collect_process_connections() -> Vec<ConnectionDetail> {
    let text = run_with_timeout("ss", &["-tunapi"], COMMAND_TIMEOUT).unwrap_or_default();
    parse_ss_connections(&text)
}

#[cfg(target_os = "macos")]
fn collect_process_connections() -> Vec<ConnectionDetail> {
    let text = run_with_timeout("lsof", &["-i", "-n", "-P", "-F", "pcPtTn"], COMMAND_TIMEOUT)
        .unwrap_or_default();
    parse_macos_lsof_connections(&text)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn collect_process_connections() -> Vec<ConnectionDetail> {
    Vec::new()
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
        // Find the `users:` field by prefix rather than positional index.
        // ss's column layout varies — kernels without `users:` (non-root),
        // distributions that surface extra fields, and ipv6 with bracketed
        // addresses can all shift the column offset. Prefix search survives
        // any column drift since the `users:` token is unambiguous.
        let (pid, process_name) = cols
            .iter()
            .find(|t| t.starts_with("users:"))
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
        // No interfaces means no throughput to allocate; any PIDs
        // returned (real conns on the test host) must therefore have
        // zero rates. We don't assert empty because the host running
        // tests typically has active sockets.
        let map = compute_approx(&[]);
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

    // ── attribution math ────────────────────────────────────────────

    fn tick(name: &str, rx_rate: f64, tx_rate: f64) -> InterfaceTick {
        InterfaceTick {
            name: name.into(),
            is_up: true,
            rx_bytes: 0,
            tx_bytes: 0,
            rx_rate,
            tx_rate,
        }
    }

    fn conn_at(name: &str, pid: u32, proto: &str, state: &str, remote: &str) -> ConnectionDetail {
        ConnectionDetail {
            protocol: proto.into(),
            local_addr: "192.168.1.5:50000".into(),
            remote_addr: remote.into(),
            state: state.into(),
            pid: Some(pid),
            process_name: Some(name.into()),
            kernel_rtt_us: None,
        }
    }

    #[test]
    fn loopback_interfaces_excluded_from_pool() {
        // 1000 B/s on en0 + 9000 B/s of local app↔db chatter on lo0.
        // The single internet-holding proc must get en0's 1000, not 10000.
        let conns = vec![conn_at("curl", 1, "TCP", "ESTABLISHED", "1.1.1.1:443")];
        let net = [tick("en0", 1000.0, 500.0), tick("lo0", 9000.0, 9000.0)];
        let map = approx_from(&conns, &net);
        let (rx, tx) = map[&1];
        assert!((rx - 1000.0).abs() < 0.01);
        assert!((tx - 500.0).abs() < 0.01);
    }

    #[test]
    fn loopback_connections_excluded_from_counts() {
        // postgres holds 8 loopback conns; curl holds the only conn
        // that can carry the wire traffic — curl gets all of it.
        let mut conns = vec![conn_at("curl", 1, "TCP", "ESTABLISHED", "1.1.1.1:443")];
        for _ in 0..8 {
            let mut c = conn_at("postgres", 2, "TCP", "ESTABLISHED", "127.0.0.1:5432");
            c.local_addr = "127.0.0.1:60000".into();
            conns.push(c);
        }
        let map = approx_from(&conns, &[tick("en0", 1000.0, 500.0)]);
        assert!((map[&1].0 - 1000.0).abs() < 0.01);
        assert!(!map.contains_key(&2));
    }

    #[test]
    fn connected_udp_counts_toward_attribution() {
        // QUIC-style: chrome holds one connected UDP flow (no state from
        // lsof), curl one TCP ESTABLISHED. They split the pool evenly.
        let conns = vec![
            conn_at("chrome", 1, "udp", "", "142.250.70.78:443"),
            conn_at("curl", 2, "TCP", "ESTABLISHED", "1.1.1.1:443"),
        ];
        let map = approx_from(&conns, &[tick("en0", 1000.0, 0.0)]);
        assert!((map[&1].0 - 500.0).abs() < 0.01);
        assert!((map[&2].0 - 500.0).abs() < 0.01);
    }

    #[test]
    fn unconnected_udp_does_not_count() {
        // mDNS-style listener with a wildcard peer earns no share.
        let conns = vec![
            conn_at("mDNSResponder", 1, "udp", "", "*:*"),
            conn_at("mDNSResponder", 2, "UDP", "UNCONN", "0.0.0.0:*"),
            conn_at("curl", 3, "TCP", "ESTABLISHED", "1.1.1.1:443"),
        ];
        let map = approx_from(&conns, &[tick("en0", 1000.0, 0.0)]);
        assert!(!map.contains_key(&1));
        assert!(!map.contains_key(&2));
        assert!((map[&3].0 - 1000.0).abs() < 0.01);
    }

    #[test]
    fn loopback_iface_names() {
        assert!(is_loopback_iface("lo"));
        assert!(is_loopback_iface("lo0"));
        assert!(is_loopback_iface("lo1"));
        assert!(!is_loopback_iface("local0"));
        assert!(!is_loopback_iface("en0"));
        assert!(!is_loopback_iface("eth0"));
    }

    #[test]
    fn loopback_addr_forms() {
        assert!(is_loopback_addr("127.0.0.1:5432"));
        assert!(is_loopback_addr("[::1]:443"));
        // The lsof parser's bracket-stripped quirk form.
        assert!(is_loopback_addr("::1]:50000"));
        assert!(!is_loopback_addr("1.1.1.1:443"));
        assert!(!is_loopback_addr("[2001:db8::1]:443"));
    }

    // ── nettop parser ───────────────────────────────────────────────

    #[test]
    fn nettop_parses_cumulative_bytes_per_pid() {
        // Captured shape from `nettop -P -J bytes_in,bytes_out -x -L 1`.
        let text = "\
time,,bytes_in,bytes_out,
19:29:46.164657,apsd.357,363400,305841,
19:29:46.164658,mDNSResponder.407,93595431,55094516,
19:29:46.164659,com.apple.WebKi.27364,78373,985908,
";
        let map = parse_nettop(text);
        assert_eq!(map[&357], (363400, 305841));
        assert_eq!(map[&407], (93595431, 55094516));
        // Dotted (truncated) names still yield the trailing pid.
        assert_eq!(map[&27364], (78373, 985908));
    }

    #[test]
    fn nettop_handles_comma_in_process_name() {
        let text = "\
time,,bytes_in,bytes_out,
19:29:46.1,weird,name.42,100,200,
";
        let map = parse_nettop(text);
        assert_eq!(map[&42], (100, 200));
    }

    #[test]
    fn nettop_skips_header_and_garbage() {
        let text = "\
time,,bytes_in,bytes_out,
not-a-row
19:29:46.1,nopid,100,200,
19:29:46.1,ok.7,x,200,
19:29:46.1,fine.9,1,2,
";
        let map = parse_nettop(text);
        assert_eq!(map.len(), 1);
        assert_eq!(map[&9], (1, 2));
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

    #[test]
    fn ss_parser_ipv6_with_brackets() {
        let text = "\
Netid State Recv-Q Send-Q Local Address:Port           Peer Address:Port  Process
tcp   ESTAB 0      0      [2001:db8::1]:443            [2001:db8::2]:55555 users:((\"nginx\",pid=99,fd=10))
";

        let conns = parse_ss_connections(text);
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].local_addr, "[2001:db8::1]:443");
        assert_eq!(conns[0].remote_addr, "[2001:db8::2]:55555");
        assert_eq!(conns[0].pid, Some(99));
        assert_eq!(conns[0].process_name.as_deref(), Some("nginx"));
    }

    #[test]
    fn ss_parser_non_root_no_users_field() {
        // ss without the `users:` field (running as non-root and without
        // CAP_NET_ADMIN) — the row should still parse, just without PID
        // or process name.
        let text = "\
Netid State Recv-Q Send-Q Local Address:Port Peer Address:Port
tcp   ESTAB 0      0      127.0.0.1:55555 93.184.216.34:443
";

        let conns = parse_ss_connections(text);
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].pid, None);
        assert_eq!(conns[0].process_name, None);
        assert_eq!(conns[0].local_addr, "127.0.0.1:55555");
    }

    #[test]
    fn ss_parser_udp_unconn_no_rtt() {
        let text = "\
Netid State  Recv-Q Send-Q Local Address:Port  Peer Address:Port  Process
udp   UNCONN 0      0      0.0.0.0:5353        0.0.0.0:*           users:((\"mDNSResponder\",pid=200,fd=15))
";

        let conns = parse_ss_connections(text);
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].protocol, "UDP");
        assert_eq!(conns[0].state, "UNCONN");
        assert_eq!(conns[0].pid, Some(200));
        assert_eq!(conns[0].kernel_rtt_us, None);
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

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_lsof_parser_multiple_connections_per_pid() {
        // One process (Chrome, pid 500) with two TCP flows. The state
        // machine must flush on each `f` boundary so both connections
        // appear in the output, sharing the PID + process name.
        let text = "\
p500
cChrome
f10
Ptcp
TST=ESTABLISHED
n10.0.0.1:55001->1.1.1.1:443
f11
Ptcp
TST=ESTABLISHED
n10.0.0.1:55002->8.8.8.8:443
";

        let conns = parse_macos_lsof_connections(text);
        assert_eq!(conns.len(), 2);
        for c in &conns {
            assert_eq!(c.pid, Some(500));
            assert_eq!(c.process_name.as_deref(), Some("Chrome"));
            assert_eq!(c.protocol, "tcp");
            assert_eq!(c.state, "ESTABLISHED");
        }
        assert_eq!(conns[0].remote_addr, "1.1.1.1:443");
        assert_eq!(conns[1].remote_addr, "8.8.8.8:443");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_lsof_parser_ipv6_with_brackets() {
        let text = "\
p700
ccurl
f8
Ptcp
TST=ESTABLISHED
n[::1]:50000->[2001:db8::1]:443
";

        let conns = parse_macos_lsof_connections(text);
        assert_eq!(conns.len(), 1);
        // Bracket stripping leaves the bare IPv6 + port.
        assert_eq!(conns[0].local_addr, "::1]:50000");
        assert_eq!(conns[0].remote_addr, "2001:db8::1]:443");
        // (The current parser only strips leading `[` / trailing `]` from
        // the whole side, not from the address half — captured here as
        // observed behaviour for now; a follow-up could tighten this.)
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_lsof_parser_listen_socket_no_arrow() {
        // A LISTEN socket has no `->` in the `n` field. The parser
        // should default remote_addr to "*:*" rather than crashing or
        // assigning the local address.
        let text = "\
p999
cnginx
f3
Ptcp
TST=LISTEN
n*:443
";

        let conns = parse_macos_lsof_connections(text);
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].state, "LISTEN");
        assert_eq!(conns[0].local_addr, "*:443");
        assert_eq!(conns[0].remote_addr, "*:*");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_lsof_parser_udp_no_state_field() {
        // UDP sockets typically don't carry a `T=ST=...` line. The
        // resulting connection should still emit with an empty state.
        let text = "\
p400
cmDNSResponder
f5
Pudp
n*:5353
";

        let conns = parse_macos_lsof_connections(text);
        assert_eq!(conns.len(), 1);
        assert_eq!(conns[0].protocol, "udp");
        assert_eq!(conns[0].state, "");
        assert_eq!(conns[0].pid, Some(400));
    }
}
