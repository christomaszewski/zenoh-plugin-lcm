use std::{
    collections::HashSet,
    future::Future,
    net::Ipv4Addr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use lcm_transport::{LcmTransport, LcmTransportConfig, LcmUrl};
use tokio::{sync::RwLock, task::JoinHandle};
use zenoh::{
    internal::{
        plugins::{Response, RunningPluginTrait, ZenohPlugin},
        runtime::DynamicRuntime,
        zerror,
    },
    key_expr::{keyexpr, KeyExpr},
    try_init_log_from_env, Result as ZResult,
};
use zenoh_plugin_trait::{plugin_long_version, plugin_version, Plugin, PluginControl};

pub mod config;
mod lcm_to_zenoh;
mod mapping;
mod zenoh_to_lcm;

use config::Config;

lazy_static::lazy_static! {
    static ref WORK_THREAD_NUM: AtomicUsize = AtomicUsize::new(config::DEFAULT_WORK_THREAD_NUM);
    static ref MAX_BLOCK_THREAD_NUM: AtomicUsize = AtomicUsize::new(config::DEFAULT_MAX_BLOCK_THREAD_NUM);
    static ref TOKIO_RUNTIME: tokio::runtime::Runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORK_THREAD_NUM.load(Ordering::SeqCst))
        .max_blocking_threads(MAX_BLOCK_THREAD_NUM.load(Ordering::SeqCst))
        .enable_all()
        .build()
        .expect("Unable to create runtime");
}

const GIT_VERSION: &str = git_version::git_version!(prefix = "v", cargo_prefix = "v");

#[inline(always)]
pub(crate) fn spawn_runtime<F>(task: F) -> JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(rt) => rt.spawn(task),
        Err(_) => TOKIO_RUNTIME.spawn(task),
    }
}

#[cfg(feature = "dynamic_plugin")]
zenoh_plugin_trait::declare_plugin!(LcmPlugin);

pub struct LcmPlugin {
    _drop: flume::Sender<()>,
    config: Mutex<Config>,
}

impl ZenohPlugin for LcmPlugin {}
impl Plugin for LcmPlugin {
    type StartArgs = DynamicRuntime;
    type Instance = zenoh::internal::plugins::RunningPlugin;

    const DEFAULT_NAME: &'static str = "lcm";
    const PLUGIN_LONG_VERSION: &'static str = plugin_long_version!();
    const PLUGIN_VERSION: &'static str = plugin_version!();

    fn start(
        name: &str,
        runtime: &Self::StartArgs,
    ) -> ZResult<zenoh::internal::plugins::RunningPlugin> {
        try_init_log_from_env();

        let runtime_conf = runtime.get_config();
        let plugin_conf = runtime_conf
            .get_plugin_config(name)
            .map_err(|_| zerror!("Plugin `{}`: missing config", name))?;
        let config: Config = serde_json::from_value(plugin_conf.clone())
            .map_err(|e| zerror!("Plugin `{}` configuration error: {}", name, e))?;

        WORK_THREAD_NUM.store(config.work_thread_num, Ordering::SeqCst);
        MAX_BLOCK_THREAD_NUM.store(config.max_block_thread_num, Ordering::SeqCst);

        let (tx, rx) = flume::bounded(0);

        let config_copy = config.clone();
        spawn_runtime(run(runtime.clone(), config, rx));

        Ok(Box::new(LcmPlugin {
            _drop: tx,
            config: Mutex::new(config_copy),
        }))
    }
}

impl PluginControl for LcmPlugin {}
impl RunningPluginTrait for LcmPlugin {
    fn adminspace_getter<'a>(
        &'a self,
        key_expr: &'a KeyExpr<'a>,
        plugin_status_key: &str,
    ) -> ZResult<Vec<Response>> {
        let mut responses = Vec::new();
        let mut key = String::from(plugin_status_key);
        with_extended_string(&mut key, &["/version"], |key| {
            if keyexpr::new(key.as_str()).unwrap().intersects(key_expr) {
                responses.push(Response::new(
                    key.clone(),
                    GIT_VERSION.into(),
                ));
            }
        });
        with_extended_string(&mut key, &["/config"], |key| {
            if keyexpr::new(key.as_str()).unwrap().intersects(key_expr) {
                if let Ok(config) = self.config.lock() {
                    responses.push(Response::new(
                        key.clone(),
                        serde_json::to_value(&*config).unwrap().into(),
                    ));
                }
            }
        });
        Ok(responses)
    }
}

async fn run(runtime: DynamicRuntime, config: Config, rx: flume::Receiver<()>) {
    try_init_log_from_env();
    tracing::debug!("LCM plugin {}", LcmPlugin::PLUGIN_LONG_VERSION);
    tracing::info!("LCM plugin {:?}", config);

    // Init Zenoh session.
    let zsession = match zenoh::session::init(runtime).await {
        Ok(session) => Arc::new(session),
        Err(e) => {
            tracing::error!("Unable to init zenoh session for LCM plugin: {:?}", e);
            return;
        }
    };

    // Create LCM transport.
    let lcm_url = match LcmUrl::parse(&config.lcm_url) {
        Ok(url) => url,
        Err(e) => {
            tracing::error!("Invalid LCM URL '{}': {}", config.lcm_url, e);
            return;
        }
    };

    let interface = config
        .network_interface
        .as_deref()
        .and_then(|iface| iface.parse::<Ipv4Addr>().ok());

    let transport_config = LcmTransportConfig {
        lcm_url,
        network_interface: interface,
        max_message_size: config.max_message_size,
        ..Default::default()
    };

    let transport = match LcmTransport::new(transport_config).await {
        Ok(t) => Arc::new(t),
        Err(e) => {
            tracing::error!("Failed to create LCM transport: {}", e);
            return;
        }
    };

    tracing::info!(
        "LCM bridge started: multicast={}, prefix='{}'",
        transport.multicast_addr(),
        config.key_prefix,
    );

    let config = Arc::new(config);

    // Shared set for loop prevention (sequence numbers sent by Zenoh→LCM).
    let sent_sequences: Arc<RwLock<HashSet<u32>>> = Arc::new(RwLock::new(HashSet::new()));

    // Spawn bidirectional bridge tasks.
    let lcm_to_zenoh_handle = spawn_runtime(lcm_to_zenoh::run(
        transport.clone(),
        zsession.clone(),
        config.clone(),
        sent_sequences.clone(),
    ));

    let zenoh_to_lcm_handle = spawn_runtime(zenoh_to_lcm::run(
        transport.clone(),
        zsession.clone(),
        config.clone(),
        sent_sequences.clone(),
    ));

    // Wait for shutdown signal.
    let _ = rx.recv_async().await;
    tracing::info!("LCM plugin shutting down");

    lcm_to_zenoh_handle.abort();
    zenoh_to_lcm_handle.abort();
}

fn with_extended_string<R, F: FnMut(&mut String) -> R>(
    prefix: &mut String,
    suffixes: &[&str],
    mut closure: F,
) -> R {
    let prefix_len = prefix.len();
    for suffix in suffixes {
        prefix.push_str(suffix);
    }
    let result = closure(prefix);
    prefix.truncate(prefix_len);
    result
}
