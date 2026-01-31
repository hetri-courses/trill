use std::path::PathBuf;
use std::sync::Arc;

use crate::trill_message_processor::CodexMessageProcessor;
use crate::trill_message_processor::CodexMessageProcessorArgs;
use crate::config_api::ConfigApi;
use crate::error_code::INVALID_REQUEST_ERROR_CODE;
use crate::outgoing_message::OutgoingMessageSender;
use async_trait::async_trait;
use trill_app_server_protocol::ChatgptAuthTokensRefreshParams;
use trill_app_server_protocol::ChatgptAuthTokensRefreshReason;
use trill_app_server_protocol::ChatgptAuthTokensRefreshResponse;
use trill_app_server_protocol::ClientInfo;
use trill_app_server_protocol::ClientRequest;
use trill_app_server_protocol::ConfigBatchWriteParams;
use trill_app_server_protocol::ConfigReadParams;
use trill_app_server_protocol::ConfigValueWriteParams;
use trill_app_server_protocol::ConfigWarningNotification;
use trill_app_server_protocol::InitializeResponse;
use trill_app_server_protocol::JSONRPCError;
use trill_app_server_protocol::JSONRPCErrorError;
use trill_app_server_protocol::JSONRPCNotification;
use trill_app_server_protocol::JSONRPCRequest;
use trill_app_server_protocol::JSONRPCResponse;
use trill_app_server_protocol::RequestId;
use trill_app_server_protocol::ServerNotification;
use trill_app_server_protocol::ServerRequestPayload;
use trill_core::AuthManager;
use trill_core::ThreadManager;
use trill_core::auth::ExternalAuthRefreshContext;
use trill_core::auth::ExternalAuthRefreshReason;
use trill_core::auth::ExternalAuthRefresher;
use trill_core::auth::ExternalAuthTokens;
use trill_core::config::Config;
use trill_core::config_loader::CloudRequirementsLoader;
use trill_core::config_loader::LoaderOverrides;
use trill_core::default_client::SetOriginatorError;
use trill_core::default_client::USER_AGENT_SUFFIX;
use trill_core::default_client::get_codex_user_agent;
use trill_core::default_client::set_default_client_residency_requirement;
use trill_core::default_client::set_default_originator;
use trill_feedback::CodexFeedback;
use trill_protocol::ThreadId;
use trill_protocol::protocol::SessionSource;
use tokio::sync::broadcast;
use tokio::time::Duration;
use tokio::time::timeout;
use toml::Value as TomlValue;

const EXTERNAL_AUTH_REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone)]
struct ExternalAuthRefreshBridge {
    outgoing: Arc<OutgoingMessageSender>,
}

impl ExternalAuthRefreshBridge {
    fn map_reason(reason: ExternalAuthRefreshReason) -> ChatgptAuthTokensRefreshReason {
        match reason {
            ExternalAuthRefreshReason::Unauthorized => ChatgptAuthTokensRefreshReason::Unauthorized,
        }
    }
}

#[async_trait]
impl ExternalAuthRefresher for ExternalAuthRefreshBridge {
    async fn refresh(
        &self,
        context: ExternalAuthRefreshContext,
    ) -> std::io::Result<ExternalAuthTokens> {
        let params = ChatgptAuthTokensRefreshParams {
            reason: Self::map_reason(context.reason),
            previous_account_id: context.previous_account_id,
        };

        let (request_id, rx) = self
            .outgoing
            .send_request_with_id(ServerRequestPayload::ChatgptAuthTokensRefresh(params))
            .await;

        let result = match timeout(EXTERNAL_AUTH_REFRESH_TIMEOUT, rx).await {
            Ok(result) => result.map_err(|err| {
                std::io::Error::other(format!("auth refresh request canceled: {err}"))
            })?,
            Err(_) => {
                let _canceled = self.outgoing.cancel_request(&request_id).await;
                return Err(std::io::Error::other(format!(
                    "auth refresh request timed out after {}s",
                    EXTERNAL_AUTH_REFRESH_TIMEOUT.as_secs()
                )));
            }
        };

        let response: ChatgptAuthTokensRefreshResponse =
            serde_json::from_value(result).map_err(std::io::Error::other)?;

        Ok(ExternalAuthTokens {
            access_token: response.access_token,
            id_token: response.id_token,
        })
    }
}

pub(crate) struct MessageProcessor {
    outgoing: Arc<OutgoingMessageSender>,
    trill_message_processor: CodexMessageProcessor,
    config_api: ConfigApi,
    config: Arc<Config>,
    initialized: bool,
    config_warnings: Vec<ConfigWarningNotification>,
}

pub(crate) struct MessageProcessorArgs {
    pub(crate) outgoing: OutgoingMessageSender,
    pub(crate) trill_linux_sandbox_exe: Option<PathBuf>,
    pub(crate) config: Arc<Config>,
    pub(crate) cli_overrides: Vec<(String, TomlValue)>,
    pub(crate) loader_overrides: LoaderOverrides,
    pub(crate) cloud_requirements: CloudRequirementsLoader,
    pub(crate) feedback: CodexFeedback,
    pub(crate) config_warnings: Vec<ConfigWarningNotification>,
}

impl MessageProcessor {
    /// Create a new `MessageProcessor`, retaining a handle to the outgoing
    /// `Sender` so handlers can enqueue messages to be written to stdout.
    pub(crate) fn new(args: MessageProcessorArgs) -> Self {
        let MessageProcessorArgs {
            outgoing,
            trill_linux_sandbox_exe,
            config,
            cli_overrides,
            loader_overrides,
            cloud_requirements,
            feedback,
            config_warnings,
        } = args;
        let outgoing = Arc::new(outgoing);
        let auth_manager = AuthManager::shared(
            config.trill_home.clone(),
            false,
            config.cli_auth_credentials_store_mode,
        );
        auth_manager.set_forced_chatgpt_workspace_id(config.forced_chatgpt_workspace_id.clone());
        auth_manager.set_external_auth_refresher(Arc::new(ExternalAuthRefreshBridge {
            outgoing: outgoing.clone(),
        }));
        let thread_manager = Arc::new(ThreadManager::new(
            config.trill_home.clone(),
            auth_manager.clone(),
            SessionSource::VSCode,
        ));
        let trill_message_processor = CodexMessageProcessor::new(CodexMessageProcessorArgs {
            auth_manager,
            thread_manager,
            outgoing: outgoing.clone(),
            trill_linux_sandbox_exe,
            config: Arc::clone(&config),
            cli_overrides: cli_overrides.clone(),
            cloud_requirements: cloud_requirements.clone(),
            feedback,
        });
        let config_api = ConfigApi::new(
            config.trill_home.clone(),
            cli_overrides,
            loader_overrides,
            cloud_requirements,
        );

        Self {
            outgoing,
            trill_message_processor,
            config_api,
            config,
            initialized: false,
            config_warnings,
        }
    }

    pub(crate) async fn process_request(&mut self, request: JSONRPCRequest) {
        let request_id = request.id.clone();
        let request_json = match serde_json::to_value(&request) {
            Ok(request_json) => request_json,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("Invalid request: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        let codex_request = match serde_json::from_value::<ClientRequest>(request_json) {
            Ok(codex_request) => codex_request,
            Err(err) => {
                let error = JSONRPCErrorError {
                    code: INVALID_REQUEST_ERROR_CODE,
                    message: format!("Invalid request: {err}"),
                    data: None,
                };
                self.outgoing.send_error(request_id, error).await;
                return;
            }
        };

        match codex_request {
            // Handle Initialize internally so CodexMessageProcessor does not have to concern
            // itself with the `initialized` bool.
            ClientRequest::Initialize { request_id, params } => {
                if self.initialized {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: "Already initialized".to_string(),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                } else {
                    let ClientInfo {
                        name,
                        title: _title,
                        version,
                    } = params.client_info;
                    if let Err(error) = set_default_originator(name.clone()) {
                        match error {
                            SetOriginatorError::InvalidHeaderValue => {
                                let error = JSONRPCErrorError {
                                    code: INVALID_REQUEST_ERROR_CODE,
                                    message: format!(
                                        "Invalid clientInfo.name: '{name}'. Must be a valid HTTP header value."
                                    ),
                                    data: None,
                                };
                                self.outgoing.send_error(request_id, error).await;
                                return;
                            }
                            SetOriginatorError::AlreadyInitialized => {
                                // No-op. This is expected to happen if the originator is already set via env var.
                                // TODO(owen): Once we remove support for CODEX_INTERNAL_ORIGINATOR_OVERRIDE,
                                // this will be an unexpected state and we can return a JSON-RPC error indicating
                                // internal server error.
                            }
                        }
                    }
                    set_default_client_residency_requirement(self.config.enforce_residency.value());
                    let user_agent_suffix = format!("{name}; {version}");
                    if let Ok(mut suffix) = USER_AGENT_SUFFIX.lock() {
                        *suffix = Some(user_agent_suffix);
                    }

                    let user_agent = get_codex_user_agent();
                    let response = InitializeResponse { user_agent };
                    self.outgoing.send_response(request_id, response).await;

                    self.initialized = true;
                    if !self.config_warnings.is_empty() {
                        for notification in self.config_warnings.drain(..) {
                            self.outgoing
                                .send_server_notification(ServerNotification::ConfigWarning(
                                    notification,
                                ))
                                .await;
                        }
                    }

                    return;
                }
            }
            _ => {
                if !self.initialized {
                    let error = JSONRPCErrorError {
                        code: INVALID_REQUEST_ERROR_CODE,
                        message: "Not initialized".to_string(),
                        data: None,
                    };
                    self.outgoing.send_error(request_id, error).await;
                    return;
                }
            }
        }

        match codex_request {
            ClientRequest::ConfigRead { request_id, params } => {
                self.handle_config_read(request_id, params).await;
            }
            ClientRequest::ConfigValueWrite { request_id, params } => {
                self.handle_config_value_write(request_id, params).await;
            }
            ClientRequest::ConfigBatchWrite { request_id, params } => {
                self.handle_config_batch_write(request_id, params).await;
            }
            ClientRequest::ConfigRequirementsRead {
                request_id,
                params: _,
            } => {
                self.handle_config_requirements_read(request_id).await;
            }
            other => {
                self.trill_message_processor.process_request(other).await;
            }
        }
    }

    pub(crate) async fn process_notification(&self, notification: JSONRPCNotification) {
        // Currently, we do not expect to receive any notifications from the
        // client, so we just log them.
        tracing::info!("<- notification: {:?}", notification);
    }

    pub(crate) fn thread_created_receiver(&self) -> broadcast::Receiver<ThreadId> {
        self.trill_message_processor.thread_created_receiver()
    }

    pub(crate) async fn try_attach_thread_listener(&mut self, thread_id: ThreadId) {
        if !self.initialized {
            return;
        }
        self.trill_message_processor
            .try_attach_thread_listener(thread_id)
            .await;
    }

    /// Handle a standalone JSON-RPC response originating from the peer.
    pub(crate) async fn process_response(&mut self, response: JSONRPCResponse) {
        tracing::info!("<- response: {:?}", response);
        let JSONRPCResponse { id, result, .. } = response;
        self.outgoing.notify_client_response(id, result).await
    }

    /// Handle an error object received from the peer.
    pub(crate) async fn process_error(&mut self, err: JSONRPCError) {
        tracing::error!("<- error: {:?}", err);
        self.outgoing.notify_client_error(err.id, err.error).await;
    }

    async fn handle_config_read(&self, request_id: RequestId, params: ConfigReadParams) {
        match self.config_api.read(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_config_value_write(
        &self,
        request_id: RequestId,
        params: ConfigValueWriteParams,
    ) {
        match self.config_api.write_value(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_config_batch_write(
        &self,
        request_id: RequestId,
        params: ConfigBatchWriteParams,
    ) {
        match self.config_api.batch_write(params).await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }

    async fn handle_config_requirements_read(&self, request_id: RequestId) {
        match self.config_api.config_requirements_read().await {
            Ok(response) => self.outgoing.send_response(request_id, response).await,
            Err(error) => self.outgoing.send_error(request_id, error).await,
        }
    }
}
