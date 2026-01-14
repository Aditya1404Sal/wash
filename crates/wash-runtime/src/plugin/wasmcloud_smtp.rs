use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

use lettre::{
    AsyncSmtpTransport, AsyncTransport, Message as LettreMessage, Tokio1Executor,
    message::Attachment,
    transport::smtp::{
        authentication::Credentials as LettreCredentials,
        client::{Tls, TlsParameters},
    },
};
use tokio::sync::RwLock;
use wasmtime::component::{HasSelf, Resource};

use crate::{
    engine::{ctx::Ctx, workload::WorkloadComponent},
    plugin::HostPlugin,
    wit::{WitInterface, WitWorld},
};

const WASMCLOUD_SMTP_ID: &str = "wasmcloud-smtp";

mod bindings {
    wasmtime::component::bindgen!({
        world: "smtp",
        imports: {
            default: async | trappable
        },
        with: {
            "wasmcloud:smtp/client/smtp-client": crate::plugin::wasmcloud_smtp::SmtpClientHandle,
        },
    });
}

/// Resource handle representing an SMTP client connection
pub type SmtpClientHandle = String;

/// Shared transport pool - multiple "clients" can reference the same transport
#[derive(Clone)]
pub struct SharedTransport {
    pub transport: Arc<AsyncSmtpTransport<Tokio1Executor>>,
    pub credentials: bindings::wasmcloud::smtp::client::Credentials,
    pub created_at: u64,
    pub connection_key: String, // Hash of host:port:username to identify unique connections
}

/// Client reference that points to a shared transport
#[derive(Clone)]
pub struct SmtpClientData {
    pub connection_key: String, // References a transport in the shared pool
    pub client_id: String,
}

/// SMTP host plugin with true connection pooling
#[derive(Clone, Default)]
pub struct WasmcloudSmtp {
    /// Shared transport pool - one transport per unique server configuration
    /// Key: connection_key (hash of host:port:username)
    transport_pool: Arc<RwLock<HashMap<String, SharedTransport>>>,

    /// Per-workload client references
    /// Key: workload_id -> client_id -> client data
    clients: Arc<RwLock<HashMap<String, HashMap<String, SmtpClientData>>>>,
}

impl WasmcloudSmtp {
    pub fn new() -> Self {
        Self {
            transport_pool: Arc::new(RwLock::new(HashMap::new())),
            clients: Arc::new(RwLock::new(HashMap::new())),
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
        credentials: &bindings::wasmcloud::smtp::client::Credentials,
    ) -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        credentials.host.hash(&mut hasher);
        credentials.port.hash(&mut hasher);
        credentials.username.hash(&mut hasher);
        credentials.password.hash(&mut hasher);
        // Add security settings to the hash
        credentials.secure.hash(&mut hasher);
        credentials.require_tls.hash(&mut hasher);

        format!("conn-{:x}", hasher.finish())
    }

    /// Generate a unique client ID
    fn generate_client_id() -> String {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut hasher = DefaultHasher::new();
        let timestamp = Self::get_timestamp();
        timestamp.hash(&mut hasher);
        uuid::Uuid::new_v4().hash(&mut hasher);

        format!("client-{:x}", hasher.finish())
    }

    /// Build an SMTP transport with proper TLS configuration
    fn build_transport(
        credentials: &bindings::wasmcloud::smtp::client::Credentials,
    ) -> anyhow::Result<AsyncSmtpTransport<Tokio1Executor>> {
        // Build TLS parameters and optionally accept invalid certs/hostnames for testing
        let mut tls_builder = TlsParameters::builder(credentials.host.clone());
        if credentials.ignore_tls.unwrap_or(false) {
            tls_builder = tls_builder.dangerous_accept_invalid_certs(true);
            // When explicitly ignoring TLS checks in tests, also allow invalid hostnames
            tls_builder = tls_builder.dangerous_accept_invalid_hostnames(true);
        }

        let tls_parameters = tls_builder.build()?;

        // Choose appropriate relay and TLS mode. Port 465 uses implicit (wrapper) TLS.
        let mut builder = match credentials.port {
            465 => AsyncSmtpTransport::<Tokio1Executor>::relay(&credentials.host)?,
            _ => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&credentials.host)?,
        };

        // For implicit TLS (port 465) use Wrapper, otherwise use Required or Opportunistic
        let tls_mode = if credentials.port == 465 {
            Tls::Wrapper(tls_parameters.clone())
        } else if credentials.require_tls.unwrap_or(false) || credentials.secure.unwrap_or(true) {
            Tls::Required(tls_parameters.clone())
        } else {
            Tls::Opportunistic(tls_parameters.clone())
        };

        builder = builder.port(credentials.port).tls(tls_mode);

        if let (Some(u), Some(p)) = (&credentials.username, &credentials.password) {
            builder = builder.credentials(LettreCredentials::new(u.clone(), p.clone()));
        }

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
        credentials: &bindings::wasmcloud::smtp::client::Credentials,
    ) -> anyhow::Result<String> {
        let connection_key = Self::generate_connection_key(credentials);

        // Check if transport already exists
        {
            let pool = self.transport_pool.read().await;
            if pool.contains_key(&connection_key) {
                tracing::debug!(
                    connection_key = connection_key,
                    host = credentials.host,
                    port = credentials.port,
                    "Reusing existing SMTP transport from pool"
                );
                return Ok(connection_key);
            }
        }

        // Create new transport
        tracing::info!(
            connection_key = connection_key,
            host = credentials.host,
            port = credentials.port,
            "Creating new SMTP transport"
        );

        let transport = Self::build_transport(credentials)?;

        // Test the connection
        transport
            .test_connection()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to connect to SMTP server: {e}"))?;

        let shared_transport = SharedTransport {
            transport: Arc::new(transport),
            credentials: credentials.clone(),
            created_at: Self::get_timestamp(),
            connection_key: connection_key.clone(),
        };

        // Store in pool
        let mut pool = self.transport_pool.write().await;
        pool.insert(connection_key.clone(), shared_transport);

        Ok(connection_key)
    }
}

#[async_trait::async_trait]
impl HostPlugin for WasmcloudSmtp {
    fn id(&self) -> &'static str {
        WASMCLOUD_SMTP_ID
    }

    fn world(&self) -> WitWorld {
        WitWorld {
            imports: HashSet::from([WitInterface::from("wasmcloud:smtp/client@0.2.0")]),
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
            .any(|i| i.namespace == "wasmcloud" && i.package == "smtp");

        if !has_smtp {
            tracing::warn!(
                "WasmcloudSmtp plugin requested for non-wasmcloud:smtp interface(s): {:?}",
                interfaces
            );
            return Ok(());
        }

        tracing::debug!(
            workload_id = component.id(),
            "Adding SMTP interface to linker for workload"
        );
        let linker = component.linker();

        bindings::wasmcloud::smtp::client::add_to_linker::<_, HasSelf<Ctx>>(linker, |ctx| ctx)?;

        let id = component.workload_id();
        tracing::debug!(
            workload_id = id,
            "Successfully added SMTP interface to linker for workload"
        );

        // Initialize client storage for this workload
        let mut clients = self.clients.write().await;
        clients.insert(id.to_string(), HashMap::new());

        tracing::debug!("WasmcloudSmtp plugin bound to workload '{id}'");

        Ok(())
    }

    async fn on_workload_unbind(
        &self,
        workload_id: &str,
        _interfaces: HashSet<crate::wit::WitInterface>,
    ) -> anyhow::Result<()> {
        // Clean up client references for this workload
        let mut clients = self.clients.write().await;
        clients.remove(workload_id);

        // Note: We don't remove transports from the pool here
        // They can be reused by other workloads with the same configuration
        // Transports will be cleaned up when the plugin is dropped

        tracing::debug!("WasmcloudSmtp plugin unbound from workload '{workload_id}'");

        Ok(())
    }
}

// Resource host trait implementation for smtp-client
impl bindings::wasmcloud::smtp::client::HostSmtpClient for Ctx {
    async fn connect(
        &mut self,
        credentials: bindings::wasmcloud::smtp::client::Credentials,
    ) -> anyhow::Result<Result<Resource<SmtpClientHandle>, String>> {
        let Some(plugin) = self.get_plugin::<WasmcloudSmtp>(WASMCLOUD_SMTP_ID) else {
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
        let client_id = WasmcloudSmtp::generate_client_id();

        // Store client reference
        let mut clients = plugin.clients.write().await;
        let workload_clients = clients.entry(self.workload_id.to_string()).or_default();

        let client_data = SmtpClientData {
            connection_key: connection_key.clone(),
            client_id: client_id.clone(),
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
        message: bindings::wasmcloud::smtp::client::Message,
    ) -> anyhow::Result<Result<bindings::wasmcloud::smtp::client::SendResult, String>> {
        let client_id = self.table.get(&client)?;

        let Some(plugin) = self.get_plugin::<WasmcloudSmtp>(WASMCLOUD_SMTP_ID) else {
            return Ok(Err("SMTP plugin not available".to_string()));
        };

        // Get client data
        let connection_key = {
            let clients = plugin.clients.read().await;
            let empty_map = HashMap::new();
            let workload_clients = clients
                .get(&self.workload_id.to_string())
                .unwrap_or(&empty_map);

            let Some(client_data) = workload_clients.get(client_id) else {
                return Ok(Err(format!("SMTP client '{client_id}' not found")));
            };

            client_data.connection_key.clone()
        };

        // Get shared transport
        let shared_transport = {
            let pool = plugin.transport_pool.read().await;
            let Some(transport) = pool.get(&connection_key) else {
                return Ok(Err(format!(
                    "SMTP transport '{}' not found",
                    connection_key
                )));
            };
            transport.clone()
        };

        // Build the email message
        let mut email_builder = LettreMessage::builder()
            .from(
                message
                    .sender
                    .from
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

        // Build the email with or without attachments
        let email = if let Some(attachments) = message.attachment {
            use lettre::message::{MultiPart, SinglePart, header::ContentType};

            let mut multipart = MultiPart::mixed().singlepart(
                SinglePart::builder()
                    .header(ContentType::TEXT_HTML)
                    .body(message.body),
            );

            // Add each attachment
            for attachment in attachments {
                // Read the file content based on the source type
                let file_content = match attachment.source {
                    bindings::wasmcloud::smtp::client::AttachmentSource::Url(url) => {
                        // Fetch from URL
                        match reqwest::get(&url).await {
                            Ok(response) => match response.bytes().await {
                                Ok(bytes) => bytes.to_vec(),
                                Err(e) => {
                                    return Ok(Err(format!(
                                        "Failed to download attachment '{}' from URL: {e}",
                                        attachment.filename
                                    )));
                                }
                            },
                            Err(e) => {
                                return Ok(Err(format!(
                                    "Failed to fetch attachment '{}' from '{}': {e}",
                                    attachment.filename, url
                                )));
                            }
                        }
                    }
                    bindings::wasmcloud::smtp::client::AttachmentSource::Path(path) => {
                        // Read from local file path
                        match tokio::fs::read(&path).await {
                            Ok(content) => content,
                            Err(e) => {
                                return Ok(Err(format!(
                                    "Failed to read attachment file '{}' from path '{}': {e}",
                                    attachment.filename, path
                                )));
                            }
                        }
                    }
                };

                // Determine content type from file extension
                let mime_type = mime_guess::from_path(&attachment.filename).first_or_octet_stream();

                let content_type = ContentType::parse(&mime_type.to_string())
                    .unwrap_or(ContentType::parse("application/octet-stream").unwrap());

                let attachment_part =
                    Attachment::new(attachment.filename).body(file_content, content_type);

                multipart = multipart.singlepart(attachment_part);
            }

            email_builder.multipart(multipart).map_err(|e| {
                anyhow::Error::msg(format!("failed to build email with attachments: {e}"))
            })?
        } else {
            email_builder
                .body(message.body)
                .map_err(|e| anyhow::Error::msg(format!("failed to build email: {e}")))?
        };

        tracing::info!(
            workload_id = self.workload_id.to_string(),
            client_id = client_id,
            connection_key = connection_key,
            subject = email
                .envelope()
                .from()
                .map(|f| f.to_string())
                .unwrap_or_default(),
            "Sending email via shared SMTP transport"
        );

        // Send using the shared transport's connection pool
        match shared_transport.transport.send(email).await {
            Ok(response) => {
                tracing::debug!(
                    workload_id = self.workload_id.to_string(),
                    client_id = client_id,
                    connection_key = connection_key,
                    response = ?response,
                    "Email sent successfully via shared transport"
                );

                Ok(Ok(bindings::wasmcloud::smtp::client::SendResult {
                    accepted: response.is_positive(),
                    server: Some(format!(
                        "{}:{}",
                        shared_transport.credentials.host, shared_transport.credentials.port
                    )),
                    message_id: {
                        let msg = response.message().collect::<Vec<_>>().join("\n");
                        if msg.is_empty() { None } else { Some(msg) }
                    },
                }))
            }
            Err(e) => {
                tracing::error!(
                    workload_id = self.workload_id.to_string(),
                    client_id = client_id,
                    connection_key = connection_key,
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

        let Some(plugin) = self.get_plugin::<WasmcloudSmtp>(WASMCLOUD_SMTP_ID) else {
            return Ok(Err("SMTP plugin not available".to_string()));
        };

        // Remove client reference (but keep shared transport in pool)
        let mut clients = plugin.clients.write().await;
        let workload_clients = clients.entry(self.workload_id.to_string()).or_default();

        if let Some(client_data) = workload_clients.remove(client_id) {
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

        let Some(plugin) = self.get_plugin::<WasmcloudSmtp>(WASMCLOUD_SMTP_ID) else {
            return Ok(());
        };

        let mut clients = plugin.clients.write().await;
        if let Some(workload_clients) = clients.get_mut(&self.workload_id.to_string()) {
            workload_clients.remove(client_id);
        }

        self.table.delete(rep)?;
        Ok(())
    }
}

impl bindings::wasmcloud::smtp::client::Host for Ctx {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_smtp_plugin_creation() {
        let smtp = WasmcloudSmtp::new();
        assert!(smtp.transport_pool.try_read().is_ok());
        assert!(smtp.clients.try_read().is_ok());
    }

    #[test]
    fn test_connection_key_consistency() {
        let creds1 = bindings::wasmcloud::smtp::client::Credentials {
            host: "smtp.gmail.com".to_string(),
            port: 587,
            username: Some("test@gmail.com".to_string()),
            password: Some("password".to_string()),
            secure: Some(false),
            ignore_tls: Some(false),
            require_tls: Some(true),
        };

        let creds2 = bindings::wasmcloud::smtp::client::Credentials {
            host: "smtp.gmail.com".to_string(),
            port: 587,
            username: Some("test@gmail.com".to_string()),
            password: Some("password".to_string()),
            secure: Some(false),
            ignore_tls: Some(false),
            require_tls: Some(true),
        };

        let key1 = WasmcloudSmtp::generate_connection_key(&creds1);
        let key2 = WasmcloudSmtp::generate_connection_key(&creds2);

        assert_eq!(
            key1, key2,
            "Same credentials should produce same connection key"
        );
    }
}
