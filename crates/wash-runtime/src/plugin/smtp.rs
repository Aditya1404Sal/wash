use dashmap::DashMap;
use html2text::from_read;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Tokio1Executor,
    message::{Attachment, MultiPart, header::ContentType},
    transport::smtp::authentication::Credentials as LettreCredentials,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::{collections::HashSet, sync::Arc};

use wasmtime::component::{HasSelf, Resource};

use crate::{
    engine::{ctx::Ctx, workload::WorkloadComponent},
    plugin::HostPlugin,
    wit::{WitInterface, WitWorld},
};

use bindings::bettyblocks::smtp::client::{
    Credentials, Host, HostSmtpClient, Message, SendResult, TlsMode, add_to_linker,
};

const BETTYBLOCKS_SMTP_PLUGIN_ID: &str = "bettyblocks-smtp";
const PLAIN_TEXT_WIDTH: usize = 80;
const MAX_POOLED_CONNECTIONS: u32 = 5;
const MIN_IDLE_CONNECTIONS: u32 = 0;

mod bindings {
    wasmtime::component::bindgen!({
        world: "smtp",
        imports: {
            default: async | trappable
        },
        with: {
            "bettyblocks:smtp/client/smtp-client": crate::plugin::smtp::SmtpClientHandle,
        },
    });
}

/// Wasmtime Resource handle that represents an SMTP client connection.(this is the connection_key)
pub type SmtpClientHandle = String;

#[derive(Clone)]
pub struct SharedTransport {
    pub transport: Arc<AsyncSmtpTransport<Tokio1Executor>>,
    pub credentials: Credentials,
    pub created_at: u64,
    /// The connection key is the hash of host:port:username:password to identify unique connections
    pub connection_key: String,
}

#[derive(Clone, Default)]
pub struct BettySmtp {
    transport_pool: Arc<DashMap<String, SharedTransport>>,
}

impl BettySmtp {
    pub fn new() -> Self {
        Self {
            transport_pool: Arc::new(DashMap::new()),
        }
    }

    fn get_timestamp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    fn generate_connection_key(credentials: &Credentials) -> String {
        let mut hasher = DefaultHasher::new();
        credentials.host.hash(&mut hasher);
        credentials.port.hash(&mut hasher);
        credentials.username.hash(&mut hasher);
        credentials.password.hash(&mut hasher);
        match credentials.tls_mode {
            TlsMode::None => 0u8.hash(&mut hasher),
            TlsMode::Starttls => 1u8.hash(&mut hasher),
            TlsMode::Implicit => 2u8.hash(&mut hasher),
        }

        format!("conn-{:x}", hasher.finish())
    }

    fn build_transport(
        credentials: &Credentials,
    ) -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
        let mut builder = match credentials.tls_mode {
            TlsMode::Implicit => AsyncSmtpTransport::<Tokio1Executor>::relay(&credentials.host)?,
            TlsMode::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&credentials.host)?
            }
            TlsMode::None => {
                AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&credentials.host)
            }
        };

        if let Some(custom_port) = credentials.port {
            builder = builder.port(custom_port);
        }

        if let (Some(username), Some(password)) = (&credentials.username, &credentials.password) {
            builder =
                builder.credentials(LettreCredentials::new(username.clone(), password.clone()));
        }

        Ok(builder
            .pool_config(
                lettre::transport::smtp::PoolConfig::new()
                    .max_size(MAX_POOLED_CONNECTIONS)
                    .min_idle(MIN_IDLE_CONNECTIONS),
            )
            .build())
    }

    async fn get_or_create_transport(&self, credentials: &Credentials) -> anyhow::Result<String> {
        let connection_key = Self::generate_connection_key(credentials);

        if self.transport_pool.contains_key(&connection_key) {
            tracing::debug!(
                connection_key = connection_key,
                host = credentials.host,
                port = credentials.port,
                "Reusing existing SMTP transport from pool"
            );
            return Ok(connection_key);
        }

        let transport = Self::build_transport(credentials)?;

        transport
            .test_connection()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to SMTP server: {e}"))?;

        tracing::info!(
            connection_key = connection_key,
            host = credentials.host,
            port = credentials.port,
            "Creating new SMTP transport"
        );

        let shared_transport = SharedTransport {
            transport: Arc::new(transport),
            credentials: credentials.clone(),
            created_at: Self::get_timestamp(),
            connection_key: connection_key.clone(),
        };

        self.transport_pool
            .entry(connection_key.clone())
            .or_insert(shared_transport);

        Ok(connection_key)
    }
}

#[async_trait::async_trait]
impl HostPlugin for BettySmtp {
    fn id(&self) -> &'static str {
        BETTYBLOCKS_SMTP_PLUGIN_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("bettyblocks:smtp/client@0.2.0")]),
            ..Default::default()
        }
    }

    async fn on_component_bind(
        &self,
        component: &mut WorkloadComponent,
        interfaces: HashSet<crate::wit::WitInterface>,
    ) -> anyhow::Result<()> {
        let has_smtp = interfaces
            .iter()
            .any(|i| i.namespace == "bettyblocks" && i.package == "smtp");

        if !has_smtp {
            tracing::warn!(
                "BettySmtp plugin requested for non-Betty:smtp interface(s): {:?}",
                interfaces
            );
            return Ok(());
        }

        tracing::debug!(
            workload_id = component.id(),
            "Adding SMTP interface to linker for workload"
        );
        let linker = component.linker();

        add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx)?;

        let id = component.workload_id();
        tracing::debug!(
            workload_id = id,
            "Successfully added SMTP interface to linker for workload"
        );

        tracing::debug!("BettySmtp plugin bound to workload '{id}'");

        Ok(())
    }

    async fn on_workload_unbind(
        &self,
        workload_id: &str,
        _interfaces: HashSet<crate::wit::WitInterface>,
    ) -> anyhow::Result<()> {
        // Note: We don't remove transports from the pool here
        // They can be reused by other workloads with the same configuration
        // Transports will be cleaned up when the plugin is dropped
        // Use disconnect to explicitly remove the transports.

        tracing::debug!("BettySmtp plugin unbound from workload '{workload_id}'");

        Ok(())
    }
}

// Resource host trait implementation for smtp-client
impl HostSmtpClient for Ctx {
    async fn connect(
        &mut self,
        credentials: Credentials,
    ) -> anyhow::Result<Result<Resource<SmtpClientHandle>, String>> {
        let Some(plugin) = self.get_plugin::<BettySmtp>(BETTYBLOCKS_SMTP_PLUGIN_ID) else {
            return Ok(Err("SMTP plugin not available".to_string()));
        };

        let connection_key = match plugin.get_or_create_transport(&credentials).await {
            Ok(key) => key,
            Err(e) => {
                return Ok(Err(format!("Failed to connect to SMTP server: {e}")));
            }
        };

        tracing::debug!(
            workload_id = self.workload_id.to_string(),
            connection_key = connection_key,
            host = credentials.host,
            port = credentials.port,
            "SMTP client connected (using shared transport)"
        );

        let resource = self.table.push(connection_key)?;
        Ok(Ok(resource))
    }

    async fn send(
        &mut self,
        client: Resource<SmtpClientHandle>,
        message: Message,
    ) -> anyhow::Result<Result<SendResult, String>> {
        let connection_key = self.table.get(&client)?;

        let Some(plugin) = self.get_plugin::<BettySmtp>(BETTYBLOCKS_SMTP_PLUGIN_ID) else {
            return Ok(Err("SMTP plugin not available".to_string()));
        };

        let Some(shared_transport) = plugin.transport_pool.get(connection_key) else {
            return Ok(Err(format!(
                "SMTP transport '{}' not found",
                connection_key
            )));
        };

        let mut email_builder = lettre::Message::builder()
            .from(
                message
                    .sender
                    .from
                    .parse()
                    .map_err(|e| anyhow::Error::msg(format!("invalid sender address: {e}")))?,
            )
            .subject(message.subject.clone());

        if let Some(reply_to) = message.sender.reply_to {
            email_builder = email_builder.reply_to(
                reply_to
                    .parse()
                    .map_err(|e| anyhow::Error::msg(format!("invalid reply-to address: {e}")))?,
            );
        }

        for to in message.recipient.to {
            email_builder = email_builder.to(to
                .parse()
                .map_err(|e| anyhow::Error::msg(format!("invalid recipient address: {e}")))?);
        }

        if let Some(cc_list) = message.recipient.cc {
            for cc in cc_list {
                email_builder = email_builder.cc(cc
                    .parse()
                    .map_err(|e| anyhow::Error::msg(format!("invalid CC address: {e}")))?);
            }
        }

        if let Some(bcc_list) = message.recipient.bcc {
            for bcc in bcc_list {
                email_builder =
                    email_builder
                        .bcc(bcc.parse().map_err(|e| {
                            anyhow::Error::msg(format!("invalid BCC address: {e}"))
                        })?);
            }
        }

        let plain_text = from_read(message.body.as_bytes(), PLAIN_TEXT_WIDTH).unwrap_or_else(|e| {
            tracing::warn!(
                "Failed to convert HTML to plain text: {}, using empty string",
                e
            );
            String::new()
        });

        let mut multipart = MultiPart::mixed()
            .multipart(MultiPart::alternative_plain_html(plain_text, message.body));

        if let Some(attachments) = message.attachments {
            for attachment in attachments {
                let content_type = ContentType::parse(&attachment.content_type)
                    .unwrap_or(ContentType::parse("application/octet-stream").unwrap());

                let attachment =
                    Attachment::new(attachment.filename).body(attachment.content, content_type);

                multipart = multipart.singlepart(attachment);
            }
        }

        let email = email_builder
            .multipart(multipart)
            .map_err(|e| anyhow::Error::msg(format!("failed to build email: {e}")))?;

        tracing::info!(
            workload_id = %self.workload_id,
            connection_key = %connection_key,
            "Sending email via shared SMTP transport"
        );

        match shared_transport.transport.send(email).await {
            Ok(response) => {
                tracing::debug!(
                    workload_id = %self.workload_id,
                    response = ?response,
                    "Email sent successfully"
                );

                let raw_msg = response.message().collect::<Vec<_>>().join(" ");
                let message_id_opt = if raw_msg.is_empty() {
                    None
                } else {
                    Some(raw_msg)
                };

                let effective_port = shared_transport.credentials.port.unwrap_or(
                    match shared_transport.credentials.tls_mode {
                        TlsMode::Implicit => 465,
                        TlsMode::Starttls => 587,
                        TlsMode::None => 25,
                    },
                );

                let server_addr = Some(format!(
                    "{}:{}",
                    shared_transport.credentials.host, effective_port
                ));

                Ok(Ok(SendResult {
                    accepted: true,
                    server: server_addr,
                    message_id: message_id_opt,
                }))
            }
            Err(e) => {
                tracing::error!(
                    workload_id = %self.workload_id,
                    error = %e,
                    "Failed to send email"
                );
                Ok(Err(format!("failed to send email: {e}")))
            }
        }
    }

    // Removes the transport from the pool to force disconnect
    async fn disconnect(
        &mut self,
        client: Resource<SmtpClientHandle>,
    ) -> anyhow::Result<Result<(), String>> {
        let connection_key = self.table.get(&client)?;

        let Some(plugin) = self.get_plugin::<BettySmtp>(BETTYBLOCKS_SMTP_PLUGIN_ID) else {
            return Ok(Err("SMTP plugin not available".to_string()));
        };

        if plugin.transport_pool.remove(connection_key).is_some() {
            tracing::debug!(
                workload_id = self.workload_id.to_string(),
                connection_key = connection_key,
                "SMTP transport forcefully disconnected and removed from pool"
            );
            Ok(Ok(()))
        } else {
            tracing::warn!(
                workload_id = self.workload_id.to_string(),
                connection_key = connection_key,
                "SMTP transport not found in pool (may have already been disconnected)"
            );
            Ok(Ok(()))
        }
    }

    async fn drop(&mut self, rep: Resource<SmtpClientHandle>) -> anyhow::Result<()> {
        let connection_key = self.table.get(&rep)?;

        tracing::debug!(
            workload_id = self.workload_id.to_string(),
            connection_key = connection_key,
            "Dropping SMTP client resource (transport remains in pool for reuse)"
        );

        // Note: we intentionally do NOT remove the transport from the pool here.
        // this allows connection pooling where subsequent requests with the same
        // credentials will reuse the existing transport.
        // Use disconnect() to explicitly close and remove the connection.

        self.table.delete(rep)?;
        Ok(())
    }
}

impl Host for Ctx {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smtp_plugin_creation() {
        let smtp = BettySmtp::new();
        assert_eq!(smtp.transport_pool.len(), 0);
    }

    #[test]
    fn test_connection_key_consistency() {
        let creds1 = Credentials {
            host: "smtp.gmail.com".to_string(),
            port: None,
            username: Some("test@gmail.com".to_string()),
            password: Some("password".to_string()),
            tls_mode: TlsMode::Implicit,
        };

        let creds2 = Credentials {
            host: "smtp.gmail.com".to_string(),
            port: None,
            username: Some("test@gmail.com".to_string()),
            password: Some("password".to_string()),
            tls_mode: TlsMode::Implicit,
        };

        let key1 = BettySmtp::generate_connection_key(&creds1);
        let key2 = BettySmtp::generate_connection_key(&creds2);

        assert_eq!(
            key1, key2,
            "Same credentials should produce same connection key"
        );
    }
}
