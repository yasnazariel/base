//! Contains the [`TxForwardingExtension`] which wires up the transaction
//! forwarding pipeline on the Base node builder.

use std::{sync::Arc, time::Duration};

use base_execution_txpool::{SpawnedConsumer, SpawnedForwarder};
use base_node_runner::{BaseNodeExtension, FromExtensionConfig, NodeHooks};
use jsonrpsee::http_client::HttpClientBuilder;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::{TxForwardingConfig, audit_dispatcher::AuditDispatcher};

/// Helper struct that wires the transaction forwarding pipeline into the node builder.
#[derive(Debug)]
pub struct TxForwardingExtension {
    /// Transaction forwarding configuration.
    pub config: TxForwardingConfig,
}

impl TxForwardingExtension {
    /// Creates a new transaction forwarding extension.
    pub const fn new(config: TxForwardingConfig) -> Self {
        Self { config }
    }
}

impl BaseNodeExtension for TxForwardingExtension {
    /// Applies the extension to the supplied hooks.
    fn apply(self: Box<Self>, hooks: NodeHooks) -> NodeHooks {
        if !self.config.enabled || self.config.builder_urls.is_empty() {
            return hooks;
        }

        let config = self.config;

        hooks.add_node_started_hook(move |ctx| {
            info!(
                builder_urls = ?config.builder_urls,
                audit_url = ?config.audit_url,
                resend_after_ms = config.resend_after_ms,
                max_batch_size = config.max_batch_size,
                max_rps = config.max_rps,
                "starting transaction forwarding pipeline"
            );

            let pool = ctx.pool().clone();
            let consumer_config = config.to_consumer_config();
            let forwarder_config = config.to_forwarder_config();
            let executor = ctx.task_executor;

            let (audit_tx, audit_dispatcher) = if let Some(audit_config) =
                config.to_audit_dispatcher_config()
            {
                let (tx, rx) = mpsc::channel(audit_config.channel_size);
                let cancel = CancellationToken::new();

                let client = HttpClientBuilder::default()
                    .request_timeout(audit_config.request_timeout)
                    .build(audit_config.audit_url.as_str())
                    .expect("valid audit URL");

                let dispatcher =
                    AuditDispatcher::new(client, rx, Arc::new(audit_config), cancel.child_token());

                let dispatcher_handle = executor.spawn_task(Box::pin(async move {
                    dispatcher.run().await;
                }));

                info!("spawned audit dispatcher");
                (Some(tx), Some((cancel, dispatcher_handle)))
            } else {
                (None, None)
            };

            let consumer = SpawnedConsumer::spawn(pool, consumer_config, &executor);
            let forwarder =
                SpawnedForwarder::spawn(&consumer.sender, forwarder_config, &executor, audit_tx);

            executor.spawn_with_graceful_shutdown_signal(|signal| {
                Box::pin(async move {
                    let _guard = signal.await;
                    consumer.shutdown();
                    forwarder.shutdown().await;
                    if let Some((cancel, handle)) = audit_dispatcher {
                        cancel.cancel();
                        let _ = tokio::time::timeout(Duration::from_secs(30), handle).await;
                    }
                })
            });

            Ok(())
        })
    }
}

impl FromExtensionConfig for TxForwardingExtension {
    type Config = TxForwardingConfig;

    fn from_config(config: Self::Config) -> Self {
        Self::new(config)
    }
}
