# Architecture Design Document

## Overview

mibee-rec is a professional laptop surveillance agent built in Rust, designed to capture local webcam and microphone audio while connecting to IP cameras and NVRs via multiple streaming protocols. The architecture prioritizes security, low resource usage, minimal dependencies, Linux-first development, and local-first deployment.

### Design Goals

- **Security first**: All external access requires encryption (TLS) and authentication. No anonymous access to streams or control surfaces.
- **Low resource usage**: Targets <5% CPU idle, <200MB RAM using zero-copy async I/O throughout the pipeline.
- **Minimal dependencies**: Prefers hand-written codec and protocol implementations. Only adds crates for genuinely hard problems (TLS, async runtime, platform ABI).
- **Linux priority**: V4L2/ALSA first-class citizens. Windows (MSMF/WASAPI) second-class. macOS not in scope.
- **Local-first**: Development, testing, and production run on the same laptop. Exact commands provided for privileged operations.

## Workspace Layout

The project uses a Rust workspace with 6 specialized crates, totaling ~22k lines of code:

```
mibee-rec/
├─ src/                # Binary entry (main.rs), config, types, error (945 LOC)
├─ crates/
│  ├─ capture/         # Video (nokhwa) + Audio (cpal) device wrappers (800 LOC)
│  ├─ protocols/       # RTSP, RTMP, ONVIF, GB28181, RTP, H.264 (11.4k LOC)
│  ├─ streaming/       # StreamHub fan-out, source/output adapters, MiBee client (4.1k LOC)
│  ├─ web/             # Axum REST API + embedded SPA + TLS (2.5k LOC)
│  ├─ security/        # Auth, TLS, encryption, rate limiting (1.9k LOC)
│  └─ observability/   # tracing + OTel + Prometheus (423 LOC)
├─ migrations/         # SQLite schema (cameras, settings, stream_sessions)
├─ config.toml         # Default runtime config
└─ tls/                # Development TLS certificates
```

## Dependency Graph

```
root (main.rs) → observability, web, security
web → security, observability, protocols  
streaming → protocols
capture → (standalone leaf)
```

## Core Abstractions

### MediaFrame

The fundamental unit of media data flowing through the pipeline:

```rust
pub enum MediaFrame {
    Video {
        keyframe: bool,        // Whether this frame is a keyframe (IDR)
        data: Vec<u8>,        // Raw NAL unit data (Annex B or AVCC format)
        timestamp: u64,       // Presentation timestamp in milliseconds
    },
    Audio {
        data: Vec<u8>,        // Raw audio data (PCM/G.711)
        timestamp: u64,       // Presentation timestamp in milliseconds
    },
}
```

### Source Trait

Async producer of media frames:

```rust
pub trait Source: Send + 'static {
    /// Start the source (open device / connect to stream)
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
    
    /// Produce the next frame (blocks asynchronously until available)
    fn next_frame(&mut self) -> Pin<Box<dyn Future<Output = Result<MediaFrame>> + Send + '_>>;
    
    /// Stop the source and release resources
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}
```

### Output Trait

Async consumer of media frames:

```rust
pub trait Output: Send + 'static {
    /// Start the output (open connection / bind listener)
    fn start(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
    
    /// Deliver a frame to the output
    fn send_frame(&mut self, frame: &MediaFrame) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
    
    /// Stop the output and release resources  
    fn stop(&mut self) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}
```

### StreamHub

Central fan-out orchestrator that connects one source to multiple outputs:

- **Broadcast channel**: Uses `tokio::sync::broadcast::channel(64)` for frame distribution
- **Decoupled processing**: Source frame rate independent from output processing speed
- **Dynamic output management**: Outputs can be added/removed while streaming
- **Task-based architecture**: Source and each output run as separate tokio tasks
- **Graceful degradation**: Slow outputs lag and potentially drop frames without blocking source

## Resource Management

The streaming pipeline integrates four complementary resource control systems:

### BufferPool

Per-stream memory pool with best-fit recycling strategy:

```rust
pub struct BufferPool {
    inner: Arc<Mutex<PoolInner>>,
    max_bytes: usize,  // 10 MB per stream budget
}
```

- **10 MB per-stream memory budget** enforced at the pool level
- **Best-fit allocation**: Finds largest available buffer ≥ required size
- **Automatic recycling**: Buffers returned to pool on drop if within budget
- **Zero-copy optimization**: Reuses allocations when possible

### ResourceController

Semaphore-based concurrency limiter:

```rust
pub struct ResourceController {
    max_streams: usize,           // Maximum concurrent streams
    semaphore: Arc<Semaphore>,    // Tracks available stream slots
}
```

- **16 concurrent streams maximum** enforced by tokio::sync::Semaphore
- **Blocking acquisition**: `acquire()` waits until slot available
- **Non-blocking alternative**: `try_acquire()` returns immediately if no slots
- **Stream permits**: Each stream holds a `StreamPermit` that releases slot on drop

### StreamBudget

Per-stream allocation tracker:

```rust
pub struct StreamBudget {
    inner: Arc<Mutex<HashMap<Uuid, usize>>>,
    max_per_stream: usize,  // 10 MB default
}
```

- **Per-stream memory tracking** prevents any single stream from consuming excessive memory
- **Reservation system**: `try_alloc()` reserves bytes before use
- **Budget enforcement**: Returns 503 error when limit exceeded
- **Automatic cleanup**: Released when streams stop or budgets removed

### StreamLifecycle

State machine for stream lifecycle management:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StreamState {
    Starting,  // Stream is being set up
    Running,   // Stream is actively running  
    Stopping,  // Stream is being torn down gracefully
    Stopped,   // Stream has stopped
    Error,     // Stream encountered an error
}
```

- **State transitions**: Enforces valid state changes (Starting → Running → Stopping → Stopped)
- **Observable states**: `StreamHandle` provides state observation and subscription
- **Error handling**: Streams in Error state can be restarted
- **Stream management**: Tracks active streams and provides lifecycle callbacks

## Stream Lifecycle State Machine

```
Starting → Running → Stopping → Stopped
   ↑                      ↓
   └──────→ Error ←───────┘
```

- **Starting**: Stream initialization, resource acquisition
- **Running**: Active frame production and distribution
- **Stopping**: Graceful shutdown, resource cleanup
- **Stopped**: Stream completed, resources freed
- **Error**: unrecoverable failure, can restart from Stopped or Error

## Data Flow

### Capture → Streaming Pipeline

```
┌─────────────┐    ┌──────────────┐    ┌─────────────┐
│   Capture   │    │   Streaming  │    │ Protocols   │
│  (nokhwa/   │──▶│   Pipeline   │──▶│ (RTSP/RTMP/ │
│  cpal)      │    │ (StreamHub)  │    │ ONVIF/GB28181│
└─────────────┘    └─────────────┘    └─────────────┘
```

**Note**: CaptureSource adapter is currently missing - the capture crate is not wired into the streaming pipeline.

### HTTP → API → Streaming → Protocol Clients

```
┌─────────┐    ┌──────────┐    ┌──────────┐    ┌─────────────┐
│  Web UI │    │  REST    │    │  Stream-  │    │ Protocol    │
│  (SPA)  │──▶│  API     │──▶│  Hub     │──▶│  Clients    │
└─────────┘    └──────────┘    └──────────┘    └─────────────┘
      ↑               │              │              │
      └─────┘        └───┬─────────┘              │
          │              │                       │
          └───────────────┼───────────────────────┘
                          │
                  ┌───────┴───────┐
                  │  Security     │
                  │  (Auth/TLS)   │
                  └───────────────┘
```

## Known Gaps

### Missing Components

1. **CaptureSource adapter**: No implementation of `Source` trait for local capture devices
2. **Streaming crate wiring**: The streaming crate is not connected to the root binary
3. **Login/Logout stubs**: Session management returns 501, actual auth flow not implemented

### Partial Implementations

1. **RTSP Server**: Structural adapter only, frame distribution not implemented
2. **RTMP Push**: Structural adapter, TCP connection and chunking not implemented
3. **GB/T 28181**: SIP/RTP transport layer stubbed, PS→H.264 conversion not implemented
4. **ONVif**: Discovery works but stream URI resolution and streaming not fully implemented

## Design Decisions

### Hand-Written Protocols

**Why**: Avoid heavy dependencies on GStreamer or similar media frameworks while maintaining full control over protocol edge cases and performance characteristics.

**Examples**: 
- RTSP/RTMP protocol handling for tricky scenarios like interleaved RTP/RTSP
- H.264 NAL unit parsing for keyframe detection and SPS/PPS extraction
- Custom buffer management to meet <200MB memory targets

### rustls over OpenSSL

**Why**: Security-first approach with modern cryptography, memory safety guarantees, and easier integration with async Rust ecosystems. OpenSSL's legacy C codebase presents greater security risks.

### Session-Based Authentication

**Why**: Stateless authentication per request would be complex with streaming protocols. Session-based auth provides a balance between security and protocol compatibility, especially for RTSP/RTMP which have their own auth mechanisms.

### Async-First Architecture

**Why**: Media streaming is inherently I/O-bound. Async I/O prevents blocking, enables better concurrency, and supports the required zero-copy optimizations for low resource usage.

## API Documentation

For detailed API documentation, see the [rustdoc output](api.md) generated by `cargo doc`:

```bash
cargo doc --open
```

## Performance Targets

| Metric | Target | Mechanism |
|--------|--------|-----------|
| CPU (idle) | <5% | Zero-copy I/O, async everywhere, no busy loops |
| Memory | <200 MB | BufferPool (10 MB pool), per-stream budgets, ResourceController |
| Concurrent streams | ≤16 | tokio::sync::Semaphore-guarded |
| First frame latency | <500 ms | Minimal buffering, eager keyframe detection |

## Testing

The architecture includes comprehensive test coverage:

- **Unit tests**: For individual components (BufferPool, StreamLifecycle, etc.)
- **Integration tests**: For MediaFrame handling and trait compliance
- **Mock sources/outputs**: For testing pipeline behavior without real hardware
- **Async test patterns**: Using tokio::test for async functionality
- **Round-trip serialization**: For all serializable types

## Resource Monitoring

Built-in memory profiling via `MemoryProfiler`:

```rust
let snapshot = MemoryProfiler::snapshot(&resource, &budget, &buffer_pool);
```

Captures RSS, buffer pool stats, stream counts, and per-stream allocations for performance tuning and debugging.