[![License](https://img.shields.io/badge/License-EPL%202.0-blue)](https://choosealicense.com/licenses/epl-2.0/)
[![License](https://img.shields.io/badge/License-Apache%202.0-blue.svg)](https://opensource.org/licenses/Apache-2.0)

# Zenoh plugin for LCM

> **Note:** This is a community-maintained project and is not affiliated with or endorsed by the [Zenoh](https://zenoh.io) or [LCM](https://lcm-proj.github.io/) teams.

A [Zenoh](https://zenoh.io) plugin and standalone bridge for [LCM](https://lcm-proj.github.io/) (Lightweight Communications and Marshalling) UDP multicast traffic.

-------------------------------

## Background

[LCM](https://lcm-proj.github.io/) is a lightweight publish/subscribe message-passing system widely used in robotics (MIT, UMich, WHOI). It uses UDP multicast for low-latency, decentralized communication — but is limited to the local network.

This plugin transparently bridges LCM multicast traffic into and out of the [Zenoh](https://zenoh.io) network, allowing existing LCM applications to benefit from Zenoh's WAN routing, wireless optimization, and security — **without any code changes** to the LCM applications.

How it works:
- An LCM publication on channel `SENSOR_IMU` is routed as a Zenoh publication on key expression `lcm/SENSOR_IMU`
- A Zenoh publication on key expression `lcm/MOTOR_CMD` is routed as an LCM multicast message on channel `MOTOR_CMD`
- Payloads are relayed **opaquely** — the bridge does not decode LCM message contents, only parses the UDP headers for the channel name and fragmentation metadata

Some examples of use cases:
- Bridging LCM traffic between robots across the internet (WAN)
- Connecting LCM-based robots to cloud services via Zenoh
- Multi-robot coordination where each robot runs LCM locally
- Recording LCM traffic via Zenoh storage backends
- Accessing LCM data via the Zenoh REST API

The plugin is available either as a dynamic library to be loaded by the Zenoh router (`zenohd`), or as a standalone executable (`zenoh-bridge-lcm`) that acts as a Zenoh peer or client.

## Architecture

```
  LCM App A                                              LCM App B
      |                                                      ^
      | UDP multicast                          UDP multicast |
      v                                                      |
┌─────────────┐      Zenoh Network       ┌─────────────┐
│ zenoh-bridge │ ◄──────────────────────► │ zenoh-bridge │
│     -lcm     │     (TCP/UDP/QUIC)      │     -lcm     │
└─────────────┘                          └─────────────┘
   Site A (LAN)                             Site B (LAN)
```

**Loop prevention** is handled via a dual mechanism:
1. **Zenoh Locality filtering**: publications from LCM use `allowed_destination(Locality::Remote)`, and the Zenoh subscriber uses `allowed_origin(Locality::Remote)`, preventing the bridge from consuming its own messages within the same Zenoh session.
2. **LCM sequence number tracking**: sequence numbers of messages sent to LCM multicast are recorded so they can be recognized and skipped when received back via multicast loopback.

## Project Structure

```
zenoh-plugin-lcm/
├── lcm-transport/          # Pure Rust LCM UDP multicast transport (no Zenoh dependency)
│   ├── src/
│   │   ├── protocol.rs     # LCM wire protocol (LC02/LC03) encode/decode
│   │   ├── fragment.rs     # Fragment reassembly with timeout and size bounds
│   │   ├── multicast.rs    # Async UDP multicast transport (tokio)
│   │   └── types.rs        # LcmMessage, LcmUrl types
│   └── tests/
│       └── multicast.rs    # Integration tests over real multicast loopback
├── zenoh-plugin-lcm/       # Zenoh plugin (cdylib + rlib)
│   └── src/
│       ├── lib.rs           # Plugin lifecycle, admin space
│       ├── config.rs        # Configuration with allow/deny regex
│       ├── lcm_to_zenoh.rs  # LCM multicast → Zenoh publisher
│       ├── zenoh_to_lcm.rs  # Zenoh subscriber → LCM multicast
│       └── mapping.rs       # Channel ↔ key expression mapping
├── zenoh-bridge-lcm/       # Standalone bridge binary
│   └── src/main.rs          # CLI with clap, RuntimeBuilder
├── DEFAULT_CONFIG.json5     # Commented default configuration
├── Dockerfile               # Multi-stage build (builder, tester, runtime)
└── docker-compose.test.yml  # One-command test execution
```

## How to Build It

> :warning: **WARNING**: Zenoh and its ecosystem are under active development. When building from the "main" branch, the plugin may not be compatible with released Zenoh binaries. Always build the plugin and zenoh router from the same branch.

1. Install [Rust](https://www.rust-lang.org/tools/install):
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

2. Clone and build:
   ```bash
   git clone https://github.com/christomaszewski/zenoh-plugin-lcm.git
   cd zenoh-plugin-lcm
   cargo build --release
   ```

3. The build produces:
   - **Standalone bridge**: `target/release/zenoh-bridge-lcm`
   - **Plugin library**: `target/release/libzenoh_plugin_lcm.so` (Linux), `.dylib` (macOS), `.dll` (Windows)

## How to Use It

### Standalone Bridge

The simplest way to run the bridge:

```bash
./target/release/zenoh-bridge-lcm
```

This starts the bridge with default settings:
- LCM multicast group: `239.255.76.67:7667` (TTL=0, localhost only)
- Zenoh key prefix: `lcm`
- Mode: Zenoh peer
- All LCM channels bridged

#### Common Examples

**Bridge LCM to a remote Zenoh router:**
```bash
zenoh-bridge-lcm -m client -e tcp/192.168.1.10:7447 --lcm-url "udpm://239.255.76.67:7667?ttl=1"
```

**Bridge only specific channels:**
```bash
zenoh-bridge-lcm --allow "SENSOR_.*|ROBOT_STATE" --deny "DEBUG_.*"
```

**Use a custom Zenoh key prefix:**
```bash
zenoh-bridge-lcm --key-prefix "robot1/lcm"
```
This maps LCM channel `SENSOR_IMU` to Zenoh key `robot1/lcm/SENSOR_IMU`.

**Bind to a specific network interface:**
```bash
zenoh-bridge-lcm --network-interface 192.168.1.100
```

**Use a configuration file:**
```bash
zenoh-bridge-lcm -c my_config.json5
```
See [`DEFAULT_CONFIG.json5`](DEFAULT_CONFIG.json5) for a fully commented example. CLI arguments override settings from the config file.

**Enable the REST API:**
```bash
zenoh-bridge-lcm --rest-http-port 8000
```
Then query the admin space:
```bash
curl -s http://localhost:8000/@/*/lcm/** | jq
```

### As a Zenoh Router Plugin

Add the plugin configuration to your zenoh router config file:

```json5
{
  plugins: {
    lcm: {
      lcm_url: "udpm://239.255.76.67:7667?ttl=1",
      key_prefix: "lcm",
      allow: "SENSOR_.*",
    }
  }
}
```

The router will automatically load `libzenoh_plugin_lcm.so` (or `.dylib`/`.dll`) if it is in the library search path.

### Docker

**Build the runtime image:**
```bash
docker build --target runtime -t zenoh-bridge-lcm .
```

**Run with host networking** (required for LCM multicast to reach host LCM applications):
```bash
docker run --rm --network host zenoh-bridge-lcm \
  --lcm-url "udpm://239.255.76.67:7667?ttl=1"
```

## Configuration

`zenoh-bridge-lcm` can be configured via a JSON5 file passed via the `-c` argument. See the commented example: [`DEFAULT_CONFIG.json5`](DEFAULT_CONFIG.json5).

The `"lcm"` part of this configuration file can also be used in the configuration file for the Zenoh router (within its `"plugins"` part).

`zenoh-bridge-lcm` also accepts the following command-line arguments. If set, each argument will override the corresponding setting from the configuration file:

### Zenoh-related arguments

- **`-c, --config <FILE>`**: A JSON5 configuration file.
- **`-m, --mode <MODE>`**: The Zenoh session mode. Default: `peer`. Possible values: `peer` or `client`. See [Zenoh documentation](https://zenoh.io/docs/getting-started/key-concepts/#deployment-units) for more details.
- **`-l, --listen <ENDPOINT>`**: A locator on which this bridge will listen for incoming sessions. Repeat this option to open several listeners. Example: `tcp/0.0.0.0:7447`.
- **`-e, --connect <ENDPOINT>`**: A peer locator this bridge will try to connect to. Repeat this option to connect to several peers. Example: `tcp/192.168.1.10:7447`.
- **`--no-multicast-scouting`**: Disable the Zenoh scouting protocol that allows automatic discovery of Zenoh peers and routers.
- **`-i, --id <HEX_STRING>`**: The identifier (as a hex string) that the bridge must use. **WARNING: this identifier must be unique in the system!** If not set, a random UUIDv4 will be used.
- **`--rest-http-port [PORT | IP:PORT]`**: Configures the HTTP interface for the REST API (disabled by default).

### LCM-related arguments

- **`--lcm-url <URL>`**: The LCM multicast URL. Default: `"udpm://239.255.76.67:7667?ttl=0"`.
  - Format: `udpm://GROUP:PORT?ttl=N&recv_buf_size=M`
  - `ttl=0`: packets stay on localhost (for local testing)
  - `ttl=1`: packets stay on the local network (typical LAN usage)
  - `recv_buf_size`: kernel UDP receive buffer size in bytes
- **`-p, --key-prefix <STRING>`**: Prefix for all Zenoh key expressions. Default: `"lcm"`. LCM channel `FOO` becomes `{prefix}/FOO` in Zenoh.
- **`-a, --allow <REGEX>`**: A regular expression matching LCM channel names to route via Zenoh. Default: all channels allowed.
- **`--deny <REGEX>`**: A regular expression matching LCM channel names to **not** route. Default: no channels denied. If both `--allow` and `--deny` are set, a channel is allowed only if it matches `--allow` and does not match `--deny`.
- **`--network-interface <IP>`**: Bind to a specific network interface by IP address for LCM multicast. Useful on multi-homed machines.
- **`--max-message-size <BYTES>`**: Maximum reassembled LCM message size in bytes. Default: `4194304` (4 MB).

### Plugin-only settings (config file only)

These settings only apply when running as a Zenoh router plugin (not the standalone bridge):

- **`work_thread_num`**: Number of worker threads in the plugin's async runtime. Default: `2`.
- **`max_block_thread_num`**: Number of blocking threads in the plugin's async runtime. Default: `50`.

## LCM Wire Protocol

The bridge implements the full LCM UDP multicast wire protocol:

| Message Type | Magic | Header Size | Description |
|---|---|---|---|
| Short (LC02) | `0x4c433032` | 8 bytes | Single-datagram messages: `magic(4) + seqno(4) + channel\0 + data` |
| Fragment (LC03) | `0x4c433033` | 20 bytes | Fragmented messages: `magic(4) + seqno(4) + msg_size(4) + offset(4) + frag_no(2) + n_frags(2)` |

Fragment reassembly features:
- Keyed by `(sender_address, sequence_number)` to safely handle multiple publishers
- Timeout-based expiry for incomplete fragment sets
- Maximum message size guard against memory exhaustion
- Duplicate fragment detection

## Testing

### Run All Tests Locally

```bash
cargo test --workspace
```

This runs **30 tests**:
- **12 unit tests** (`lcm-transport`): wire protocol encoding/decoding, fragment reassembly, URL parsing
- **7 integration tests** (`lcm-transport`): real UDP multicast send/receive over loopback, including fragmented messages and multi-transport scenarios
- **11 unit tests** (`zenoh-plugin-lcm`): configuration parsing, channel/key-expression mapping, allow/deny filtering

The integration tests use multicast with TTL=0 (localhost only) and unique ports per test, so they work on any system with a loopback interface and require no special privileges.

### Run Tests in Docker

```bash
docker compose -f docker-compose.test.yml up --build --exit-code-from test
```

This builds the project and runs the full test suite inside a container.

### Run Only Integration Tests

```bash
cargo test -p lcm-transport --test multicast
```

### Run Only Unit Tests

```bash
cargo test -p lcm-transport --lib
cargo test -p zenoh-plugin-lcm --lib
```

## Admin Space

The bridge exposes status information via the Zenoh admin space. If the REST plugin is enabled (`--rest-http-port`), you can query it:

```bash
# Get bridge version
curl -s http://localhost:8000/@/*/lcm/version | jq

# Get current configuration
curl -s http://localhost:8000/@/*/lcm/config | jq
```

## License

This project is dual-licensed under the [Eclipse Public License 2.0](https://www.eclipse.org/legal/epl-2.0/) and the [Apache License 2.0](https://www.apache.org/licenses/LICENSE-2.0). You may use this software under the terms of either license.
