use std::{collections::HashSet, sync::Arc};

use dashmap::DashMap;
use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message as LettreMessage, Tokio1Executor,
    message::{Attachment, MultiPart, SinglePart, header::ContentType},
    transport::smtp::{
        authentication::Credentials as LettreCredentials,
        client::{Tls, TlsParameters},
    },
};
use wasmtime::component::{HasSelf, Resource};

use crate::{
    engine::{ctx::Ctx, workload::WorkloadComponent},
    plugin::HostPlugin,
    wit::{WitInterface, WitWorld},
};

const BETTYBLOCKS_SMTP_ID: &str = "bettyblocks-smtp";

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

/// Wasmtime Resource handle that represents an SMTP client connection
pub type SmtpClientHandle = String;

/// Shared transport pool - multiple "clients" can reference the same transport
/// the connection key is the Hash of host:port:username to identify unique connections
#[derive(Clone)]
pub struct SharedTransport {
    pub transport: Arc<AsyncSmtpTransport<Tokio1Executor>>,
    pub credentials: bindings::bettyblocks::smtp::client::Credentials,
    pub created_at: u64,
    pub connection_key: String,
}

#[derive(Clone)]
pub struct SmtpClientData {
    pub connection_key: String,
}

/// SMTP host plugin (with connection pooling)
#[derive(Clone, Default)]
pub struct BettySmtp {
    /// Shared transport pool - one transport per unique server configuration
    /// Key: connection_key (hash of host:port:username)
    transport_pool: Arc<DashMap<String, SharedTransport>>,

    /// Per-workload client references
    /// Key: workload_id -> (client_id -> client data)
    clients: Arc<DashMap<String, DashMap<String, SmtpClientData>>>,
}

impl BettySmtp {
    pub fn new() -> Self {
        Self {
            transport_pool: Arc::new(DashMap::new()),
            clients: Arc::new(DashMap::new()),
        }
    }

    fn get_timestamp() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    /// Generate a unique connection key based on server configuration
    /// Multiple clients with same config will share the same transport
    fn generate_connection_key(
        credentials: &bindings::bettyblocks::smtp::client::Credentials,
    ) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        credentials.host.hash(&mut hasher);
        credentials.port.hash(&mut hasher);
        credentials.username.hash(&mut hasher);
        credentials.password.hash(&mut hasher);
        credentials.implicit_tls.hash(&mut hasher);

        format!("conn-{:x}", hasher.finish())
    }

    fn generate_client_id() -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        let timestamp = Self::get_timestamp();
        timestamp.hash(&mut hasher);
        uuid::Uuid::new_v4().hash(&mut hasher);

        format!("client-{:x}", hasher.finish())
    }

    fn build_transport(
        credentials: &bindings::bettyblocks::smtp::client::Credentials,
    ) -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
        let tls_builder = TlsParameters::builder(credentials.host.clone());
        let tls_parameters = tls_builder.build()?;

        let mut builder = match credentials.implicit_tls {
            true => AsyncSmtpTransport::<Tokio1Executor>::relay(&credentials.host)?,
            false => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&credentials.host)?,
        };

        let tls_mode = if credentials.implicit_tls {
            Tls::Wrapper(tls_parameters)
        } else {
            Tls::Required(tls_parameters)
        };

        builder = builder.tls(tls_mode);

        if let Some(custom_port) = credentials.port {
            builder = builder.port(custom_port);
        }

        builder = builder.credentials(LettreCredentials::new(
            credentials.username.clone(),
            credentials.password.clone(),
        ));

        Ok(builder
            .pool_config(
                lettre::transport::smtp::PoolConfig::new()
                    .max_size(20)
                    .min_idle(5),
            )
            .build())
    }

    /// Get or create a shared transport for the given credentials
    async fn get_or_create_transport(
        &self,
        credentials: &bindings::bettyblocks::smtp::client::Credentials,
    ) -> anyhow::Result<String> {
        let connection_key = Self::generate_connection_key(credentials);

        // Check if transport already exists
        if self.transport_pool.contains_key(&connection_key) {
            tracing::debug!(
                connection_key = connection_key,
                host = credentials.host,
                port = credentials.port,
                "Reusing existing SMTP transport from pool"
            );
            return Ok(connection_key);
        }

        // Create new transport
        let transport = Self::build_transport(credentials)?;

        // Test the connection
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

        // Insert only if not already present
        self.transport_pool
            .entry(connection_key.clone())
            .or_insert(shared_transport);

        Ok(connection_key)
    }
}

#[async_trait::async_trait]
impl HostPlugin for BettySmtp {
    fn id(&self) -> &'static str {
        BETTYBLOCKS_SMTP_ID
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

        bindings::bettyblocks::smtp::client::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx)?;

        let id = component.workload_id();
        tracing::debug!(
            workload_id = id,
            "Successfully added SMTP interface to linker for workload"
        );

        // Initialize client storage for this workload
        self.clients.insert(id.to_string(), DashMap::new());

        tracing::debug!("BettySmtp plugin bound to workload '{id}'");

        Ok(())
    }

    async fn on_workload_unbind(
        &self,
        workload_id: &str,
        _interfaces: HashSet<crate::wit::WitInterface>,
    ) -> anyhow::Result<()> {
        // Clean up client references for this workload
        self.clients.remove(workload_id);

        // Note: We don't remove transports from the pool here
        // They can be reused by other workloads with the same configuration
        // Transports will be cleaned up when the plugin is dropped

        tracing::debug!("BettySmtp plugin unbound from workload '{workload_id}'");

        Ok(())
    }
}

// Resource host trait implementation for smtp-client
impl bindings::bettyblocks::smtp::client::HostSmtpClient for Ctx {
    async fn connect(
        &mut self,
        credentials: bindings::bettyblocks::smtp::client::Credentials,
    ) -> anyhow::Result<Result<Resource<SmtpClientHandle>, String>> {
        let Some(plugin) = self.get_plugin::<BettySmtp>(BETTYBLOCKS_SMTP_ID) else {
            return Ok(Err("SMTP plugin not available".to_string()));
        };

        // Get or create shared transport
        let connection_key = match plugin.get_or_create_transport(&credentials).await {
            Ok(key) => key,
            Err(e) => {
                return Ok(Err(format!("Failed to connect to SMTP server: {e}")));
            }
        };

        // Generate unique client ID
        let client_id = BettySmtp::generate_client_id();

        // Store client reference
        let Some(workload_clients) = plugin.clients.get(&self.workload_id.to_string()) else {
            return Ok(Err(format!("Workload '{}' not found", self.workload_id)));
        };

        let client_data = SmtpClientData {
            connection_key: connection_key.clone(),
        };

        workload_clients.insert(client_id.clone(), client_data);

        tracing::debug!(
            workload_id = self.workload_id.to_string(),
            client_id = client_id,
            connection_key = connection_key,
            host = credentials.host,
            port = credentials.port,
            "SMTP client connected (using shared transport)"
        );

        let resource = self.table.push(client_id)?;
        Ok(Ok(resource))
    }

    async fn send(
        &mut self,
        client: Resource<SmtpClientHandle>,
        message: bindings::bettyblocks::smtp::client::Message,
    ) -> anyhow::Result<Result<bindings::bettyblocks::smtp::client::SendResult, String>> {
        let client_id = self.table.get(&client)?;

        let Some(plugin) = self.get_plugin::<BettySmtp>(BETTYBLOCKS_SMTP_ID) else {
            return Ok(Err("SMTP plugin not available".to_string()));
        };

        let connection_key = {
            let Some(workload_clients) = plugin.clients.get(&self.workload_id.to_string()) else {
                return Ok(Err(format!("Workload '{}' not found", self.workload_id)));
            };

            let Some(client_data) = workload_clients.get(client_id) else {
                return Ok(Err(format!("SMTP client '{client_id}' not found")));
            };

            client_data.connection_key.clone()
        };

        let Some(shared_transport) = plugin.transport_pool.get(&connection_key) else {
            return Ok(Err(format!(
                "SMTP transport '{}' not found",
                connection_key
            )));
        };

        let mut email_builder = LettreMessage::builder()
            .from(
                message
                    .sender
                    .address
                    .parse()
                    .map_err(|e| anyhow::Error::msg(format!("invalid sender address: {e}")))?,
            )
            .subject(message.subject);

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

        let email = if let Some(attachments) = message.attachments {
            let mut multipart = MultiPart::mixed().singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(message.body),
            );

            for attachment in attachments {
                let content_type = ContentType::parse(&attachment.content_type)
                    .unwrap_or(ContentType::parse("application/octet-stream").unwrap());

                let attachment_part =
                    Attachment::new(attachment.filename).body(attachment.content, content_type);

                multipart = multipart.singlepart(attachment_part);
            }

            email_builder.multipart(multipart).map_err(|e| {
                anyhow::Error::msg(format!("failed to build email with attachments: {e}"))
            })?
        } else {
            email_builder
                .header(ContentType::TEXT_HTML)
                .body(message.body)
                .map_err(|e| anyhow::Error::msg(format!("failed to build email: {e}")))?
        };

        tracing::info!(
            workload_id = %self.workload_id,
            client_id = %client_id,
            connection_key = ?connection_key,
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
                    if shared_transport.credentials.implicit_tls {
                        465
                    } else {
                        587
                    },
                );

                let server_addr = Some(format!(
                    "{}:{}",
                    shared_transport.credentials.host, effective_port
                ));

                Ok(Ok(bindings::bettyblocks::smtp::client::SendResult {
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

    async fn disconnect(
        &mut self,
        client: Resource<SmtpClientHandle>,
    ) -> anyhow::Result<Result<(), String>> {
        let client_id = self.table.get(&client)?;

        let Some(plugin) = self.get_plugin::<BettySmtp>(BETTYBLOCKS_SMTP_ID) else {
            return Ok(Err("SMTP plugin not available".to_string()));
        };

        let Some(workload_clients) = plugin.clients.get(&self.workload_id.to_string()) else {
            return Ok(Err(format!("Workload '{}' not found", self.workload_id)));
        };

        if let Some((_, client_data)) = workload_clients.remove(client_id) {
            tracing::debug!(
                workload_id = self.workload_id.to_string(),
                client_id = client_id,
                connection_key = client_data.connection_key,
                "SMTP client disconnected (shared transport remains in pool)"
            );
            Ok(Ok(()))
        } else {
            Ok(Err(format!("SMTP client '{client_id}' not found")))
        }
    }

    async fn drop(&mut self, rep: Resource<SmtpClientHandle>) -> anyhow::Result<()> {
        let client_id = self.table.get(&rep)?;

        tracing::debug!(
            workload_id = self.workload_id.to_string(),
            client_id = client_id,
            resource_id = ?rep,
            "Dropping SMTP client resource"
        );

        let Some(plugin) = self.get_plugin::<BettySmtp>(BETTYBLOCKS_SMTP_ID) else {
            return Ok(());
        };

        if let Some(workload_clients) = plugin.clients.get(&self.workload_id.to_string()) {
            workload_clients.remove(client_id);
        }

        self.table.delete(rep)?;
        Ok(())
    }
}

impl bindings::bettyblocks::smtp::client::Host for Ctx {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smtp_plugin_creation() {
        let smtp = BettySmtp::new();
        assert_eq!(smtp.transport_pool.len(), 0);
        assert_eq!(smtp.clients.len(), 0);
    }

    #[test]
    fn test_connection_key_consistency() {
        let creds1 = bindings::bettyblocks::smtp::client::Credentials {
            host: "smtp.gmail.com".to_string(),
            port: None,
            username: "test@gmail.com".to_string(),
            password: "password".to_string(),
            implicit_tls: true,
        };

        let creds2 = bindings::bettyblocks::smtp::client::Credentials {
            host: "smtp.gmail.com".to_string(),
            port: None,
            username: "test@gmail.com".to_string(),
            password: "password".to_string(),
            implicit_tls: true,
        };

        let key1 = BettySmtp::generate_connection_key(&creds1);
        let key2 = BettySmtp::generate_connection_key(&creds2);

        assert_eq!(
            key1, key2,
            "Same credentials should produce same connection key"
        );
    }
}
