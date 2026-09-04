//! Observability endpoints (SPEC v1 §3.2): the real-time resource summary,
//! the log-ring reader, and the request-trace ring.
//!
//! Real-time only — the sampler keeps the previous sample for rate
//! computation and the rings are bounded, so nothing grows with uptime.
//! `/proc` parsers are pure functions over file contents (unit-testable on
//! any Linux machine).

use axum::Json;
use axum::response::IntoResponse;
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

// ─────────────────────────────────────────────────────────────────────────
// /proc parsers (pure)
// ─────────────────────────────────────────────────────────────────────────

/// Cumulative CPU times from `/proc/stat`'s aggregate `cpu` line, in ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuTimes {
    pub idle: u64,
    pub total: u64,
}

/// Parse the aggregate `cpu` line of `/proc/stat` (idle includes iowait).
#[must_use]
pub fn parse_cpu_stat(content: &str) -> Option<CpuTimes> {
    let line = content.lines().find(|l| l.starts_with("cpu "))?;
    let vals: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .filter_map(|v| v.parse::<u64>().ok())
        .collect();
    if vals.is_empty() {
        return None;
    }
    let idle = vals.get(3).copied().unwrap_or(0) + vals.get(4).copied().unwrap_or(0);
    let total: u64 = vals.iter().sum();
    Some(CpuTimes { idle, total })
}

/// CPU busy-percent between two samples (0..100; 0 without elapsed time).
#[must_use]
pub fn cpu_percent_between(prev: CpuTimes, cur: CpuTimes) -> f64 {
    let d_total = cur.total.saturating_sub(prev.total);
    let d_idle = cur.idle.saturating_sub(prev.idle);
    if d_total == 0 {
        return 0.0;
    }
    ((d_total - d_idle) as f64 / d_total as f64 * 100.0).clamp(0.0, 100.0)
}

/// (MemTotal, MemAvailable) in bytes from `/proc/meminfo`.
#[must_use]
pub fn parse_meminfo(content: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        content.lines().find_map(|l| {
            let rest = l.strip_prefix(name)?;
            let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
            Some(kb * 1024)
        })
    };
    Some((field("MemTotal:")?, field("MemAvailable:").unwrap_or(0)))
}

/// Aggregate (rx, tx) bytes over physical interfaces from `/proc/net/dev`.
#[must_use]
pub fn parse_net_dev(content: &str) -> (u64, u64) {
    let mut rx = 0;
    let mut tx = 0;
    for line in content.lines().skip(2) {
        let Some((iface, data)) = line.split_once(':') else {
            continue;
        };
        if iface.trim() == "lo" {
            continue;
        }
        let vals: Vec<u64> = data
            .split_whitespace()
            .filter_map(|v| v.parse::<u64>().ok())
            .collect();
        rx += vals.first().copied().unwrap_or(0);
        tx += vals.get(8).copied().unwrap_or(0);
    }
    (rx, tx)
}

/// (utime, stime) ticks from `/proc/<pid>/stat` (comm may contain spaces —
/// split after the last `)`).
#[must_use]
pub fn parse_self_stat(content: &str) -> Option<(u64, u64)> {
    let rest = content.rsplit_once(')')?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    let utime: u64 = fields.get(11)?.parse().ok()?;
    let stime: u64 = fields.get(12)?.parse().ok()?;
    Some((utime, stime))
}

/// (rchar, wchar) from `/proc/<pid>/io`.
#[must_use]
pub fn parse_self_io(content: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        content.lines().find_map(|l| {
            let rest = l.strip_prefix(name)?;
            rest.trim().parse::<u64>().ok()
        })
    };
    Some((field("rchar:")?, field("wchar:")?))
}

/// Process CPU percent between tick samples against `num_cpus`.
#[must_use]
pub fn proc_cpu_percent(prev: (u64, u64), cur: (u64, u64), dt_secs: f64, num_cpus: f64) -> f64 {
    if dt_secs <= 0.0 || num_cpus <= 0.0 {
        return 0.0;
    }
    let d_ticks = cur.0.saturating_sub(prev.0) as f64 + cur.1.saturating_sub(prev.1) as f64;
    (d_ticks / 100.0 / dt_secs * 100.0 / num_cpus).clamp(0.0, 100.0 * num_cpus)
}

// ─────────────────────────────────────────────────────────────────────────
// Sampler state
// ─────────────────────────────────────────────────────────────────────────

/// One raw sampling point (cumulative counters).
#[derive(Debug, Clone, Copy, Default)]
pub struct Sample {
    ts_ms: u64,
    cpu: CpuTimes,
    proc_ticks: (u64, u64),
    net: (u64, u64),
}

/// Rendered snapshot served by `GET /api/metrics/summary` (SPEC §3.2).
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub ts: u64,
    pub interval_ms: u64,
    pub system_cpu_percent: f64,
    pub load_avg: [f64; 3],
    pub mem_total: u64,
    pub mem_available: u64,
    pub disks: Vec<(String, u64, u64, u64)>,
    pub net_rx: u64,
    pub net_tx: u64,
    pub net_rx_rate: f64,
    pub net_tx_rate: f64,
    pub proc_cpu_percent: f64,
    pub rss_bytes: u64,
    pub open_fds: u64,
    pub io_read_bytes: u64,
    pub io_write_bytes: u64,
}

/// Read one raw sample from the live system.
#[must_use]
pub fn read_sample() -> Sample {
    let ts_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    let cpu = std::fs::read_to_string("/proc/stat")
        .ok()
        .as_deref()
        .and_then(parse_cpu_stat)
        .unwrap_or_default();
    let proc_ticks = std::fs::read_to_string("/proc/self/stat")
        .ok()
        .and_then(|s| parse_self_stat(&s))
        .unwrap_or((0, 0));
    let net = std::fs::read_to_string("/proc/net/dev")
        .map(|s| parse_net_dev(&s))
        .unwrap_or((0, 0));
    Sample {
        ts_ms,
        cpu,
        proc_ticks,
        net,
    }
}

fn read_rss_bytes() -> u64 {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines().find_map(|l| {
                let rest = l.strip_prefix("VmRSS:")?;
                let kb: u64 = rest.split_whitespace().next()?.parse().ok()?;
                Some(kb * 1024)
            })
        })
        .unwrap_or(0)
}

fn count_open_fds() -> u64 {
    std::fs::read_dir("/proc/self/fd")
        .map(|entries| entries.filter_map(|e| e.ok()).count() as u64)
        .unwrap_or(0)
}

fn statvfs(path: &str) -> Option<(u64, u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(std::path::Path::new(path).as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    let frsize = st.f_frsize as u64;
    let total = st.f_blocks * frsize;
    let free = st.f_bfree * frsize;
    let used = total.saturating_sub(st.f_bavail * frsize);
    Some((total, used, free))
}

// ─────────────────────────────────────────────────────────────────────────
// Shared state (process-global: one web server per process)
// ─────────────────────────────────────────────────────────────────────────

/// One traced Web API request (SPEC §3.2 `/api/requests`).
#[derive(Debug, Clone, Serialize)]
pub struct RequestEntry {
    pub id: String,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub duration_ms: f64,
    pub ts: u64,
}

/// Observability state: sampler snapshot, request ring, traffic counters.
pub struct Observe {
    snapshot: Mutex<Snapshot>,
    prev: Mutex<Option<Sample>>,
    requests: Mutex<VecDeque<RequestEntry>>,
    pub http_rx: AtomicU64,
    pub http_tx: AtomicU64,
    pub rtsp_tx: AtomicU64,
    pub gb28181_tx: AtomicU64,
    next_request_id: AtomicU64,
    request_cap: usize,
    started: SystemTime,
}

const REQUEST_CAP: usize = 500;
const SAMPLER_INTERVAL: Duration = Duration::from_secs(2);

static OBSERVE: OnceLock<Arc<Observe>> = OnceLock::new();

/// The process-global observability state.
#[must_use]
pub fn observe() -> &'static Arc<Observe> {
    OBSERVE.get_or_init(|| {
        Arc::new(Observe {
            snapshot: Mutex::new(Snapshot::default()),
            prev: Mutex::new(None),
            requests: Mutex::new(VecDeque::with_capacity(REQUEST_CAP)),
            http_rx: AtomicU64::new(0),
            http_tx: AtomicU64::new(0),
            rtsp_tx: AtomicU64::new(0),
            gb28181_tx: AtomicU64::new(0),
            next_request_id: AtomicU64::new(1),
            request_cap: REQUEST_CAP,
            started: SystemTime::now(),
        })
    })
}

impl Observe {
    /// Record one raw sample and render the snapshot (real /proc reads plus
    /// previous-sample deltas).
    pub fn record_sample(&self, cur: Sample, num_cpus: f64) {
        let prev = self.prev.lock().unwrap().replace(cur);
        let dt_secs = match prev {
            Some(p) => cur.ts_ms.saturating_sub(p.ts_ms).max(1),
            None => 0,
        } as f64
            / 1000.0;

        let mut snap = self.snapshot.lock().unwrap();
        snap.ts = cur.ts_ms;
        snap.interval_ms = (dt_secs * 1000.0).round() as u64;
        if let Some(prev) = prev {
            snap.system_cpu_percent = cpu_percent_between(prev.cpu, cur.cpu);
            snap.proc_cpu_percent =
                proc_cpu_percent(prev.proc_ticks, cur.proc_ticks, dt_secs, num_cpus);
            snap.net_rx_rate = (cur.net.0.saturating_sub(prev.net.0)) as f64 / dt_secs;
            snap.net_tx_rate = (cur.net.1.saturating_sub(prev.net.1)) as f64 / dt_secs;
        }
        snap.net_rx = cur.net.0;
        snap.net_tx = cur.net.1;

        if let Some((total, avail)) = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .as_deref()
            .and_then(parse_meminfo)
        {
            snap.mem_total = total;
            snap.mem_available = avail;
        }
        if let Ok(avg) = std::fs::read_to_string("/proc/loadavg") {
            let vals: Vec<f64> = avg
                .split_whitespace()
                .filter_map(|v| v.parse().ok())
                .collect();
            for (slot, v) in snap.load_avg.iter_mut().zip(vals) {
                *slot = v;
            }
        }
        let mut disks = Vec::new();
        if let Some((total, used, free)) = statvfs("/") {
            disks.push(("/".to_string(), total, used, free));
        }
        snap.disks = disks;
        snap.rss_bytes = read_rss_bytes();
        snap.open_fds = count_open_fds();
        if let Some((r, w)) = std::fs::read_to_string("/proc/self/io")
            .ok()
            .as_deref()
            .and_then(parse_self_io)
        {
            snap.io_read_bytes = r;
            snap.io_write_bytes = w;
        }
    }

    /// Latest rendered snapshot.
    #[must_use]
    pub fn snapshot(&self) -> Snapshot {
        self.snapshot.lock().unwrap().clone()
    }

    fn push_request(&self, entry: RequestEntry) {
        let mut guard = self.requests.lock().unwrap();
        if guard.len() == self.request_cap {
            guard.pop_front();
        }
        guard.push_back(entry);
    }

    /// Recent request traces, newest first.
    #[must_use]
    pub fn requests_newest_first(&self) -> Vec<RequestEntry> {
        let guard = self.requests.lock().unwrap();
        guard.iter().rev().cloned().collect()
    }

    fn alloc_request_id(&self) -> String {
        format!(
            "{:06x}",
            self.next_request_id.fetch_add(1, Ordering::Relaxed)
        )
    }
}

/// Background sampler: refresh the shared snapshot every 2s (SPEC §3.2).
pub fn spawn_sampler() {
    let observe = observe().clone();
    tokio::spawn(async move {
        let cpus = std::thread::available_parallelism()
            .map(|n| n.get() as f64)
            .unwrap_or(1.0);
        let mut ticker = tokio::time::interval(SAMPLER_INTERVAL);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            ticker.tick().await;
            observe.record_sample(read_sample(), cpus);
        }
    });
}

// ─────────────────────────────────────────────────────────────────────────
// Middleware & handlers
// ─────────────────────────────────────────────────────────────────────────

/// Request middleware (SPEC §3.2): request id (echoed via `X-Request-Id`),
/// trace entry per `/api` call, and app-attributed HTTP traffic counters.
pub(crate) async fn observe_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::HeaderValue;

    let observe = observe();
    let id = observe.alloc_request_id();
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let rx: u64 = request
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    observe.http_rx.fetch_add(rx, Ordering::Relaxed);

    let start = std::time::Instant::now();
    let mut response = next.run(request).await;
    let elapsed = start.elapsed();

    let tx: u64 = response
        .headers()
        .get(axum::http::header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    observe.http_tx.fetch_add(tx, Ordering::Relaxed);

    if path.starts_with("/api/") {
        observe.push_request(RequestEntry {
            id: id.clone(),
            method: method.to_string(),
            path: path.clone(),
            status: response.status().as_u16(),
            duration_ms: elapsed.as_secs_f64() * 1000.0,
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });
    }
    if let Ok(v) = HeaderValue::from_str(&id) {
        response.headers_mut().insert("x-request-id", v);
    }
    response
}

fn level_rank(level: &str) -> u8 {
    observability::log_ring::level_rank(level)
}

/// `GET /api/metrics/summary` — real-time system + process snapshot.
pub async fn metrics_summary() -> impl IntoResponse {
    let s = observe().snapshot();
    let o = observe();
    Json(serde_json::json!({
        "ok": true,
        "data": {
            "ts": s.ts,
            "interval_ms": s.interval_ms,
            "system": {
                "cpu_percent": s.system_cpu_percent,
                "load_avg": s.load_avg,
                "memory": {
                    "total": s.mem_total,
                    "used": s.mem_total.saturating_sub(s.mem_available),
                    "available": s.mem_available,
                },
                "disks": s.disks.iter().map(|(p, t, u, f)| serde_json::json!({
                    "path": p, "total": t, "used": u, "free": f,
                })).collect::<Vec<_>>(),
                "network": {
                    "rx_bytes": s.net_rx, "tx_bytes": s.net_tx,
                    "rx_rate": s.net_rx_rate, "tx_rate": s.net_tx_rate,
                },
            },
            "process": {
                "cpu_percent": s.proc_cpu_percent,
                "rss_bytes": s.rss_bytes,
                "open_fds": s.open_fds,
                "uptime": o.started.elapsed().map(|d| d.as_secs()).unwrap_or(0),
                "io_read_bytes": s.io_read_bytes,
                "io_write_bytes": s.io_write_bytes,
                "storage_bytes": 0u64,
                "traffic": {
                    "http_rx_bytes": o.http_rx.load(Ordering::Relaxed),
                    "http_tx_bytes": o.http_tx.load(Ordering::Relaxed),
                    "rtsp_tx_bytes": o.rtsp_tx.load(Ordering::Relaxed),
                    "gb28181_tx_bytes": o.gb28181_tx.load(Ordering::Relaxed),
                },
            },
        },
    }))
}

/// `GET /api/logs?limit=&level=` — the tracing log ring, newest first.
pub async fn logs_handler(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(200)
        .clamp(1, 1000);
    let min_level = params.get("level").map(|l| level_rank(l)).unwrap_or(0);
    let entries = observability::log_ring::newest_first(limit, min_level);
    Json(serde_json::json!({ "ok": true, "data": { "entries": entries } }))
}

/// `GET /api/requests?limit=` — recent request traces, newest first.
pub async fn requests_handler(
    axum::extract::Query(params): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let limit = params
        .get("limit")
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(100)
        .clamp(1, 500);
    let entries = observe().requests_newest_first();
    let entries: Vec<_> = entries.into_iter().take(limit).collect();
    Json(serde_json::json!({ "ok": true, "data": { "entries": entries } }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[test]
    fn test_parse_cpu_stat_aggregate_includes_iowait_in_idle() {
        let src = "cpu  100 0 100 300 100 0 0 0 0 0\ncpu0 50 0 50 150 50 0 0 0 0 0\nintr 123\n";
        let t = parse_cpu_stat(src).expect("parses");
        assert_eq!(t.idle, 400); // 300 idle + 100 iowait
        assert_eq!(t.total, 600);
    }

    #[test]
    fn test_cpu_percent_between() {
        let prev = CpuTimes {
            idle: 400,
            total: 600,
        };
        let cur = CpuTimes {
            idle: 550,
            total: 800,
        };
        assert!((cpu_percent_between(prev, cur) - 25.0).abs() < 1e-9);
        assert_eq!(cpu_percent_between(prev, prev), 0.0);
    }

    #[test]
    fn test_parse_meminfo() {
        let src =
            "MemTotal:       8000000 kB\nMemFree:        100000 kB\nMemAvailable:   3000000 kB\n";
        let (total, avail) = parse_meminfo(src).expect("parses");
        assert_eq!(total, 8_000_000 * 1024);
        assert_eq!(avail, 3_000_000 * 1024);
    }

    #[test]
    fn test_parse_net_dev_skips_loopback() {
        let src = "Inter-|   Receive  |  Transmit\n\
                   face |bytes packets errs drop fifo frame compressed multicast|bytes packets errs drop fifo colls carrier\n\
                    lo:  999999 999 0 0 0 0 0 0 9999 999 0 0 0 0 0 0\n\
                   eth0: 150000 200 0 0 0 0 0 0 90000 150 0 0 0 0 0 0\n";
        let (rx, tx) = parse_net_dev(src);
        assert_eq!(rx, 150_000);
        assert_eq!(tx, 90_000);
    }

    #[test]
    fn test_parse_self_stat_with_spaces_in_comm() {
        let src = "42 (camera worke) S 1 2 3 0 -1 4194560 100 0 0 0 77 33 0 0 20 0 4 0 123456 1 1";
        let (utime, stime) = parse_self_stat(src).expect("parses");
        assert_eq!(utime, 77);
        assert_eq!(stime, 33);
    }

    #[test]
    fn test_parse_self_io() {
        let src = "rchar: 123456\nwchar: 654321\nsyscr: 100\n";
        let (r, w) = parse_self_io(src).expect("parses");
        assert_eq!(r, 123_456);
        assert_eq!(w, 654_321);
    }

    #[test]
    fn test_proc_cpu_percent_scales_by_cores() {
        assert_eq!(proc_cpu_percent((0, 0), (100, 0), 1.0, 1.0), 100.0);
        assert_eq!(proc_cpu_percent((0, 0), (100, 0), 1.0, 4.0), 25.0);
    }

    #[test]
    fn test_record_sample_snapshot_shape() {
        let observe = observe();
        observe.record_sample(read_sample(), 4.0);
        std::thread::sleep(Duration::from_millis(20));
        observe.record_sample(read_sample(), 4.0);
        let s = observe.snapshot();
        assert!(s.ts > 0);
        assert!(s.mem_total > 0, "meminfo parsed");
        assert!(s.rss_bytes > 0, "VmRSS parsed");
        assert!(s.disks.iter().any(|(p, _, _, _)| p == "/"));
    }

    #[tokio::test]
    async fn test_summary_and_logs_handlers_shape() {
        let app = axum::Router::new()
            .route("/api/metrics/summary", axum::routing::get(metrics_summary))
            .route("/api/logs", axum::routing::get(logs_handler))
            .route("/api/requests", axum::routing::get(requests_handler));
        for uri in ["/api/metrics/summary", "/api/logs", "/api/requests"] {
            let res = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK, "{uri}");
            let body = axum::body::to_bytes(res.into_body(), usize::MAX)
                .await
                .unwrap();
            let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(json["ok"], true, "{uri}");
        }
        let res = app
            .oneshot(
                Request::builder()
                    .uri("/api/metrics/summary")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let body = axum::body::to_bytes(res.into_body(), usize::MAX)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let data = &json["data"];
        assert!(data["system"]["memory"]["total"].as_u64().unwrap_or(0) > 0);
        assert!(data["process"]["traffic"]["http_tx_bytes"].is_u64());
    }
}
