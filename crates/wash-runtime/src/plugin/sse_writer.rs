//! SSE Writer plugin — allows components to push events to open SSE connections.
//!
//! Implements `bettyblocks:sse/handler@0.1.0` by delegating to the host-level
//! [`SseRegistry`] which holds the actual stream senders.

use std::collections::HashSet;
use std::sync::Arc;

use wasmtime::component::HasSelf;

use crate::{
    engine::{ctx::Ctx, workload::WorkloadComponent},
    host::sse::SseRegistry,
    plugin::HostPlugin,
    wit::{WitInterface, WitWorld},
};

use bindings::bettyblocks::sse::handler::{Host, add_to_linker};

const SSE_WRITER_PLUGIN_ID: &str = "bettyblocks-sse-writer";

mod bindings {
    wasmtime::component::bindgen!({
        world: "sse-writer",
        imports: {
            default: async | trappable
        },
    });
}

#[derive(Clone)]
pub struct SseWriterPlugin {
    registry: Arc<SseRegistry>,
}

impl SseWriterPlugin {
    pub fn new(registry: Arc<SseRegistry>) -> Self {
        Self { registry }
    }
}

#[async_trait::async_trait]
impl HostPlugin for SseWriterPlugin {
    fn id(&self) -> &'static str {
        SSE_WRITER_PLUGIN_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("bettyblocks:sse/handler@0.1.0")]),
            ..Default::default()
        }
    }

    async fn on_component_bind(
        &self,
        component: &mut WorkloadComponent,
        interfaces: HashSet<WitInterface>,
    ) -> anyhow::Result<()> {
        let has_sse = interfaces
            .iter()
            .any(|i| i.namespace == "bettyblocks" && i.package == "sse");

        if !has_sse {
            tracing::warn!(
                "SseWriter plugin requested for non-bettyblocks:sse interface(s): {:?}",
                interfaces
            );
            return Ok(());
        }

        tracing::debug!(
            workload_id = component.id(),
            "Adding SSE writer interface to linker"
        );

        let linker = component.linker();
        add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx)?;

        tracing::debug!(
            workload_id = component.id(),
            "SSE writer plugin bound to workload"
        );

        Ok(())
    }

    async fn on_workload_unbind(
        &self,
        workload_id: &str,
        _interfaces: HashSet<WitInterface>,
    ) -> anyhow::Result<()> {
        tracing::debug!("SseWriter plugin unbound from workload '{workload_id}'");
        Ok(())
    }
}

impl Host for Ctx {
    async fn write_event(
        &mut self,
        stream_id: String,
        data: String,
    ) -> anyhow::Result<Result<(), String>> {
        let Some(plugin) = self.get_plugin::<SseWriterPlugin>(SSE_WRITER_PLUGIN_ID) else {
            return Ok(Err("SSE writer plugin not available".to_string()));
        };

        match plugin.registry.write_event(&stream_id, &data).await {
            Ok(()) => {
                tracing::debug!(
                    stream_id = %stream_id,
                    workload_id = self.workload_id.to_string(),
                    "SSE event written"
                );
                Ok(Ok(()))
            }
            Err(e) => Ok(Err(e)),
        }
    }

    async fn is_registered(&mut self, stream_id: String) -> anyhow::Result<bool> {
        let Some(plugin) = self.get_plugin::<SseWriterPlugin>(SSE_WRITER_PLUGIN_ID) else {
            return Ok(false);
        };

        Ok(plugin.registry.is_registered(&stream_id).await)
    }

    async fn list_connections(
        &mut self,
        scope_key: String,
    ) -> anyhow::Result<Vec<String>> {
        let Some(plugin) = self.get_plugin::<SseWriterPlugin>(SSE_WRITER_PLUGIN_ID) else {
            return Ok(Vec::new());
        };

        Ok(plugin.registry.list_connections(&scope_key).await)
    }
}
