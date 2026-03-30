use std::str::FromStr;

use clap::{App, Arg};
use zenoh::{
    config::Config,
    init_log_from_env_or,
    internal::{plugins::PluginsManager, runtime::RuntimeBuilder},
    session::ZenohId,
};
use zenoh_config::ModeDependentValue;
use zenoh_plugin_trait::Plugin;

macro_rules! insert_json5 {
    ($config: expr, $args: expr, $key: expr, if $name: expr) => {
        if $args.occurrences_of($name) > 0 {
            $config.insert_json5($key, "true").unwrap();
        }
    };
    ($config: expr, $args: expr, $key: expr, if $name: expr, $($t: tt)*) => {
        if $args.occurrences_of($name) > 0 {
            $config.insert_json5($key, &serde_json::to_string(&$args.value_of($name).unwrap()$($t)*).unwrap()).unwrap();
        }
    };
    ($config: expr, $args: expr, $key: expr, for $name: expr, $($t: tt)*) => {
        if let Some(value) = $args.values_of($name) {
            $config.insert_json5($key, &serde_json::to_string(&value$($t)*).unwrap()).unwrap();
        }
    };
}

fn parse_args() -> Config {
    let app = App::new("zenoh bridge for LCM")
        .version(zenoh_plugin_lcm::LcmPlugin::PLUGIN_VERSION)
        .long_version(zenoh_plugin_lcm::LcmPlugin::PLUGIN_LONG_VERSION)
        //
        // zenoh related arguments:
        //
        .arg(Arg::from_usage(
r"-i, --id=[HEX_STRING] \
'The identifier (as an hexadecimal string, with odd number of chars - e.g.: 0A0B23...) that zenohd must use.
WARNING: this identifier must be unique in the system and must be 16 bytes maximum (32 chars)!
If not set, a random UUIDv4 will be used.'",
            ))
        .arg(Arg::from_usage(
r#"-m, --mode=[MODE]  'The zenoh session mode.'"#)
            .possible_values(["peer", "client"])
            .default_value("peer")
        )
        .arg(Arg::from_usage(
r"-c, --config=[FILE] \
'The configuration file. Currently, this file must be a valid JSON5 file.'",
            ))
        .arg(Arg::from_usage(
r"-l, --listen=[ENDPOINT]... \
'A locator on which this router will listen for incoming sessions.
Repeat this option to open several listeners.'",
                ),
            )
        .arg(Arg::from_usage(
r"-e, --connect=[ENDPOINT]... \
'A peer locator this router will try to connect to.
Repeat this option to connect to several peers.'",
            ))
        .arg(Arg::from_usage(
r"--no-multicast-scouting \
'By default the zenoh bridge listens and replies to UDP multicast scouting messages for being discovered by peers and routers.
This option disables this feature.'"
        ))
        .arg(Arg::from_usage(
r"--rest-http-port=[PORT | IP:PORT] \
'Configures HTTP interface for the REST API (disabled by default, setting this option enables it). Accepted values:'
  - a port number
  - a string with format `<local_ip>:<port_number>` (to bind the HTTP server to a specific interface)."
        ))
        //
        // LCM related arguments:
        //
        .arg(Arg::from_usage(
r#"--lcm-url=[URL] \
'The LCM multicast URL. Default: "udpm://239.255.76.67:7667?ttl=0".
Format: udpm://GROUP:PORT?ttl=N&recv_buf_size=M'"#
        ))
        .arg(Arg::from_usage(
r#"-p, --key-prefix=[STRING] \
'A string used as prefix for all Zenoh key expressions mapped from LCM channels. Default: "lcm".
LCM channel "FOO" becomes "{prefix}/FOO" in Zenoh.'"#
        ))
        .arg(Arg::from_usage(
r#"-a, --allow=[STRING] \
'A regular expression matching the LCM channel names that must be routed via Zenoh. By default all channels are allowed.
If both --allow and --deny are set, a channel is allowed only if it matches the allow expression and does not match the deny expression.'"#
        ))
        .arg(Arg::from_usage(
r#"--deny=[STRING] \
'A regular expression matching the LCM channel names that must NOT be routed via Zenoh. By default no channels are denied.'"#
        ))
        .arg(Arg::from_usage(
r#"--network-interface=[IP] \
'Bind to a specific network interface (by IP address) for LCM multicast. Useful on multi-homed machines.'"#
        ))
        .arg(Arg::from_usage(
r#"--max-message-size=[BYTES] \
'Maximum reassembled LCM message size in bytes. Default: 4194304 (4 MB).'"#
        ));

    let args = app.get_matches();

    // Load config file first.
    let mut config = match args.value_of("config") {
        Some(conf_file) => Config::from_file(conf_file).unwrap(),
        None => Config::default(),
    };
    // If "lcm" plugin conf is not present, add it (empty to use defaults).
    if config.plugin("lcm").is_none() {
        config.insert_json5("plugins/lcm", "{}").unwrap();
    }

    // Apply zenoh related arguments over config.
    if args.occurrences_of("id") > 0 {
        config
            .set_id(Some(
                ZenohId::from_str(args.value_of("id").unwrap()).unwrap(),
            ))
            .unwrap();
    }
    if args.occurrences_of("mode") > 0 {
        config
            .set_mode(Some(args.value_of("mode").unwrap().parse().unwrap()))
            .unwrap();
    }
    if let Some(endpoints) = args.values_of("connect") {
        config
            .connect
            .endpoints
            .set(endpoints.map(|p| p.parse().unwrap()).collect())
            .unwrap();
    }
    if let Some(endpoints) = args.values_of("listen") {
        config
            .listen
            .endpoints
            .set(endpoints.map(|p| p.parse().unwrap()).collect())
            .unwrap();
    }
    if args.is_present("no-multicast-scouting") {
        config.scouting.multicast.set_enabled(Some(false)).unwrap();
    }
    if let Some(port) = args.value_of("rest-http-port") {
        config
            .insert_json5("plugins/rest/http_port", &format!(r#""{port}""#))
            .unwrap();
    }

    // Enable timestamping.
    config
        .timestamping
        .set_enabled(Some(ModeDependentValue::Unique(true)))
        .unwrap();

    // Enable admin space.
    config.adminspace.set_enabled(true).unwrap();
    // Enable loading plugins.
    config.plugins_loading.set_enabled(true).unwrap();

    // Apply LCM related arguments over config.
    insert_json5!(config, args, "plugins/lcm/lcm_url", if "lcm-url",);
    insert_json5!(config, args, "plugins/lcm/key_prefix", if "key-prefix",);
    insert_json5!(config, args, "plugins/lcm/allow", if "allow",);
    insert_json5!(config, args, "plugins/lcm/deny", if "deny",);
    insert_json5!(config, args, "plugins/lcm/network_interface", if "network-interface",);
    if args.occurrences_of("max-message-size") > 0 {
        config
            .insert_json5(
                "plugins/lcm/max_message_size",
                args.value_of("max-message-size").unwrap(),
            )
            .unwrap();
    }

    config
}

#[tokio::main]
async fn main() {
    init_log_from_env_or("z=info");

    tracing::info!(
        "zenoh-bridge-lcm {}",
        zenoh_plugin_lcm::LcmPlugin::PLUGIN_LONG_VERSION,
    );

    let config = parse_args();
    tracing::info!("Zenoh {config:?}");

    let mut plugins_mgr = PluginsManager::static_plugins_only();

    // Declare REST plugin if specified in conf.
    if config.plugin("rest").is_some() {
        plugins_mgr.declare_static_plugin::<zenoh_plugin_rest::RestPlugin, &str>("rest", true);
    }

    // Declare LCM plugin.
    plugins_mgr.declare_static_plugin::<zenoh_plugin_lcm::LcmPlugin, &str>("lcm", true);

    // Create a zenoh Runtime.
    let mut runtime = match RuntimeBuilder::new(config)
        .plugins_manager(plugins_mgr)
        .build()
        .await
    {
        Ok(runtime) => runtime,
        Err(e) => {
            println!("{e}. Exiting...");
            std::process::exit(-1);
        }
    };
    if let Err(e) = runtime.start().await {
        println!("Failed to start Zenoh runtime: {e}. Exiting...");
        std::process::exit(-1);
    }

    futures::future::pending::<()>().await;
}
