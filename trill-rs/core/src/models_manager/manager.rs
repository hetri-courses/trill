use super::cache::ModelsCacheManager;
use crate::api_bridge::auth_provider_from_auth;
use crate::api_bridge::map_api_error;
use crate::auth::AuthManager;
use crate::auth::AuthMode;
use crate::config::Config;
use crate::default_client::build_reqwest_client;
use crate::error::CodexErr;
use crate::error::Result as CoreResult;
use crate::features::Feature;
use crate::model_provider_info::ModelProviderInfo;
use crate::model_provider_info::LMSTUDIO_OSS_PROVIDER_ID;
use crate::model_provider_info::OLLAMA_CHAT_PROVIDER_ID;
use crate::model_provider_info::OLLAMA_OSS_PROVIDER_ID;
use crate::models_manager::collaboration_mode_presets::builtin_collaboration_mode_presets;
use crate::models_manager::model_info;
use crate::models_manager::model_presets::builtin_model_presets;
use trill_api::ModelsClient;
use trill_api::ReqwestTransport;
use trill_protocol::config_types::CollaborationModeMask;
use trill_protocol::openai_models::ConfigShellToolType;
use trill_protocol::openai_models::ModelInfo;
use trill_protocol::openai_models::ModelPreset;
use trill_protocol::openai_models::ModelVisibility;
use trill_protocol::openai_models::ModelsResponse;
use trill_protocol::openai_models::TruncationPolicyConfig;
use http::HeaderMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::RwLock;
use tokio::sync::TryLockError;
use tokio::time::timeout;
use tracing::error;
use tracing::warn;

const MODEL_CACHE_FILE: &str = "models_cache.json";
const DEFAULT_MODEL_CACHE_TTL: Duration = Duration::from_secs(300);
const MODELS_REFRESH_TIMEOUT: Duration = Duration::from_secs(5);

/// Strategy for refreshing available models.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshStrategy {
    /// Always fetch from the network, ignoring cache.
    Online,
    /// Only use cached data, never fetch from the network.
    Offline,
    /// Use cache if available and fresh, otherwise fetch from the network.
    OnlineIfUncached,
}

/// Default context window for OSS models when not specified in config.
const DEFAULT_OSS_CONTEXT_WINDOW: i64 = 8192;

/// Coordinates remote model discovery plus cached metadata on disk.
#[derive(Debug)]
pub struct ModelsManager {
    local_models: Vec<ModelPreset>,
    remote_models: RwLock<Vec<ModelInfo>>,
    auth_manager: Arc<AuthManager>,
    etag: RwLock<Option<String>>,
    cache_manager: ModelsCacheManager,
    provider: ModelProviderInfo,
    /// The provider ID (e.g., "openai", "lmstudio", "ollama").
    model_provider_id: String,
}

impl ModelsManager {
    /// Construct a manager scoped to the provided `AuthManager`.
    ///
    /// Uses `trill_home` to store cached model metadata and initializes with built-in presets.
    /// The `model_provider_id` is used to determine whether to fetch models from OSS providers
    /// (like LM Studio or Ollama) or from the OpenAI API.
    pub fn new(
        trill_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        model_provider_id: String,
    ) -> Self {
        let cache_path = trill_home.join(MODEL_CACHE_FILE);
        let cache_manager = ModelsCacheManager::new(cache_path, DEFAULT_MODEL_CACHE_TTL);
        Self {
            local_models: builtin_model_presets(auth_manager.get_internal_auth_mode()),
            remote_models: RwLock::new(Self::load_remote_models_from_file().unwrap_or_default()),
            auth_manager,
            etag: RwLock::new(None),
            cache_manager,
            provider: ModelProviderInfo::create_openai_provider(),
            model_provider_id,
        }
    }

    /// Check if the current provider is an OSS provider (LM Studio, Ollama, etc.).
    fn is_oss_provider(&self) -> bool {
        matches!(
            self.model_provider_id.as_str(),
            LMSTUDIO_OSS_PROVIDER_ID | OLLAMA_OSS_PROVIDER_ID | OLLAMA_CHAT_PROVIDER_ID
        )
    }

    /// List all available models, refreshing according to the specified strategy.
    ///
    /// Returns model presets sorted by priority and filtered by auth mode and visibility.
    pub async fn list_models(
        &self,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> Vec<ModelPreset> {
        if let Err(err) = self
            .refresh_available_models(config, refresh_strategy)
            .await
        {
            error!("failed to refresh available models: {err}");
        }
        let remote_models = self.get_remote_models(config).await;
        self.build_available_models(remote_models)
    }

    /// List collaboration mode presets.
    ///
    /// Returns a static set of presets seeded with the configured model.
    pub fn list_collaboration_modes(&self) -> Vec<CollaborationModeMask> {
        builtin_collaboration_mode_presets()
    }

    /// Attempt to list models without blocking, using the current cached state.
    ///
    /// Returns an error if the internal lock cannot be acquired.
    pub fn try_list_models(&self, config: &Config) -> Result<Vec<ModelPreset>, TryLockError> {
        let remote_models = self.try_get_remote_models(config)?;
        Ok(self.build_available_models(remote_models))
    }

    // todo(aibrahim): should be visible to core only and sent on session_configured event
    /// Get the model identifier to use, refreshing according to the specified strategy.
    ///
    /// If `model` is provided, returns it directly. Otherwise selects the default based on
    /// auth mode and available models.
    pub async fn get_default_model(
        &self,
        model: &Option<String>,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> String {
        if let Some(model) = model.as_ref() {
            return model.to_string();
        }
        if let Err(err) = self
            .refresh_available_models(config, refresh_strategy)
            .await
        {
            error!("failed to refresh available models: {err}");
        }
        let remote_models = self.get_remote_models(config).await;
        let available = self.build_available_models(remote_models);
        available
            .iter()
            .find(|model| model.is_default)
            .or_else(|| available.first())
            .map(|model| model.model.clone())
            .unwrap_or_default()
    }

    // todo(aibrahim): look if we can tighten it to pub(crate)
    /// Look up model metadata, applying remote overrides and config adjustments.
    pub async fn get_model_info(&self, model: &str, config: &Config) -> ModelInfo {
        let remote = self
            .get_remote_models(config)
            .await
            .into_iter()
            .find(|m| m.slug == model);
        let model = if let Some(remote) = remote {
            remote
        } else {
            model_info::find_model_info_for_slug(model)
        };
        model_info::with_config_overrides(model, config)
    }

    /// Refresh models if the provided ETag differs from the cached ETag.
    ///
    /// Uses `Online` strategy to fetch latest models when ETags differ.
    pub(crate) async fn refresh_if_new_etag(&self, etag: String, config: &Config) {
        let current_etag = self.get_etag().await;
        if current_etag.clone().is_some() && current_etag.as_deref() == Some(etag.as_str()) {
            if let Err(err) = self.cache_manager.renew_cache_ttl().await {
                error!("failed to renew cache TTL: {err}");
            }
            return;
        }
        if let Err(err) = self
            .refresh_available_models(config, RefreshStrategy::Online)
            .await
        {
            error!("failed to refresh available models: {err}");
        }
    }

    /// Refresh available models according to the specified strategy.
    async fn refresh_available_models(
        &self,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> CoreResult<()> {
        // For OSS providers, use the dedicated OSS refresh logic.
        if self.is_oss_provider() {
            return self.refresh_oss_models(config, refresh_strategy).await;
        }

        if !config.features.enabled(Feature::RemoteModels)
            || self.auth_manager.get_internal_auth_mode() == Some(AuthMode::ApiKey)
        {
            return Ok(());
        }

        match refresh_strategy {
            RefreshStrategy::Offline => {
                // Only try to load from cache, never fetch
                self.try_load_cache().await;
                Ok(())
            }
            RefreshStrategy::OnlineIfUncached => {
                // Try cache first, fall back to online if unavailable
                if self.try_load_cache().await {
                    return Ok(());
                }
                self.fetch_and_update_models().await
            }
            RefreshStrategy::Online => {
                // Always fetch from network
                self.fetch_and_update_models().await
            }
        }
    }

    /// Refresh models from OSS providers (LM Studio, Ollama, etc.).
    async fn refresh_oss_models(
        &self,
        config: &Config,
        refresh_strategy: RefreshStrategy,
    ) -> CoreResult<()> {
        match refresh_strategy {
            RefreshStrategy::Offline => {
                // For offline, just use what we have cached in memory
                Ok(())
            }
            RefreshStrategy::OnlineIfUncached | RefreshStrategy::Online => {
                // Fetch models from the OSS provider
                match self.fetch_oss_models(config).await {
                    Ok(models) => {
                        *self.remote_models.write().await = models;
                        Ok(())
                    }
                    Err(e) => {
                        warn!("Failed to fetch models from OSS provider: {e}");
                        // Don't treat this as fatal - we can still work with cached/default models
                        Ok(())
                    }
                }
            }
        }
    }

    /// Fetch models from an OSS provider (LM Studio or Ollama).
    async fn fetch_oss_models(&self, config: &Config) -> CoreResult<Vec<ModelInfo>> {
        let provider = config
            .model_providers
            .get(&self.model_provider_id)
            .ok_or_else(|| {
                CodexErr::InvalidRequest(format!(
                    "Provider {} not found in model_providers",
                    self.model_provider_id
                ))
            })?;

        let base_url = provider.base_url.as_ref().ok_or_else(|| {
            CodexErr::InvalidRequest(format!(
                "Provider {} has no base_url configured",
                self.model_provider_id
            ))
        })?;

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let url = format!("{}/models", base_url.trim_end_matches('/'));
        let response = timeout(MODELS_REFRESH_TIMEOUT, client.get(&url).send())
            .await
            .map_err(|_| CodexErr::Timeout)?
            .map_err(|e| CodexErr::InvalidRequest(format!("Failed to fetch models: {e}")))?;

        if !response.status().is_success() {
            return Err(CodexErr::InvalidRequest(format!(
                "Failed to fetch models: HTTP {}",
                response.status()
            )));
        }

        let json: serde_json::Value = response
            .json::<serde_json::Value>()
            .await
            .map_err(|e| CodexErr::InvalidRequest(format!("Failed to parse models response: {e}")))?;

        let model_ids: Vec<String> = json["data"]
            .as_array()
            .ok_or_else(|| CodexErr::InvalidRequest("No 'data' array in models response".into()))?
            .iter()
            .filter_map(|model| model["id"].as_str())
            .map(String::from)
            .collect();

        let models: Vec<ModelInfo> = model_ids
            .into_iter()
            .map(|id| self.create_oss_model_info(&id, config))
            .collect();

        Ok(models)
    }

    /// Create a ModelInfo for an OSS model.
    fn create_oss_model_info(&self, model_id: &str, config: &Config) -> ModelInfo {
        // Check for per-model settings in config
        let model_settings = config.model_settings.get(model_id);

        // Determine context window: per-model setting > global setting > default
        let context_window = model_settings
            .and_then(|s| s.context_window)
            .or(config.model_context_window)
            .unwrap_or(DEFAULT_OSS_CONTEXT_WINDOW);

        // Determine auto-compact token limit: per-model setting > global setting > 90% of context
        let auto_compact_token_limit = model_settings
            .and_then(|s| s.auto_compact_token_limit)
            .or(config.model_auto_compact_token_limit)
            .unwrap_or_else(|| (context_window * 90) / 100);

        // Format display name from model ID (e.g., "qwen/qwen2.5-coder-14b" -> "qwen2.5-coder-14b")
        let display_name = model_id
            .split('/')
            .last()
            .unwrap_or(model_id)
            .to_string();

        ModelInfo {
            slug: model_id.to_string(),
            display_name,
            description: Some(format!("Local model via {}", self.model_provider_id)),
            default_reasoning_level: None,
            supported_reasoning_levels: Vec::new(),
            shell_type: ConfigShellToolType::Default,
            visibility: ModelVisibility::List,
            supported_in_api: true,
            priority: 0,
            upgrade: None,
            base_instructions: model_info::BASE_INSTRUCTIONS.to_string(),
            model_messages: None,
            supports_reasoning_summaries: false,
            support_verbosity: false,
            default_verbosity: None,
            apply_patch_tool_type: None,
            truncation_policy: TruncationPolicyConfig::bytes(10_000),
            supports_parallel_tool_calls: false,
            context_window: Some(context_window),
            auto_compact_token_limit: Some(auto_compact_token_limit),
            effective_context_window_percent: 95,
            experimental_supported_tools: Vec::new(),
        }
    }

    async fn fetch_and_update_models(&self) -> CoreResult<()> {
        let _timer =
            trill_otel::start_global_timer("codex.remote_models.fetch_update.duration_ms", &[]);
        let auth = self.auth_manager.auth().await;
        let auth_mode = self.auth_manager.get_internal_auth_mode();
        let api_provider = self.provider.to_api_provider(auth_mode)?;
        let api_auth = auth_provider_from_auth(auth.clone(), &self.provider)?;
        let transport = ReqwestTransport::new(build_reqwest_client());
        let client = ModelsClient::new(transport, api_provider, api_auth);

        let client_version = format_client_version_to_whole();
        let (models, etag) = timeout(
            MODELS_REFRESH_TIMEOUT,
            client.list_models(&client_version, HeaderMap::new()),
        )
        .await
        .map_err(|_| CodexErr::Timeout)?
        .map_err(map_api_error)?;

        self.apply_remote_models(models.clone()).await;
        *self.etag.write().await = etag.clone();
        self.cache_manager.persist_cache(&models, etag).await;
        Ok(())
    }

    async fn get_etag(&self) -> Option<String> {
        self.etag.read().await.clone()
    }

    /// Replace the cached remote models and rebuild the derived presets list.
    async fn apply_remote_models(&self, models: Vec<ModelInfo>) {
        let mut existing_models = Self::load_remote_models_from_file().unwrap_or_default();
        for model in models {
            if let Some(existing_index) = existing_models
                .iter()
                .position(|existing| existing.slug == model.slug)
            {
                existing_models[existing_index] = model;
            } else {
                existing_models.push(model);
            }
        }
        *self.remote_models.write().await = existing_models;
    }

    fn load_remote_models_from_file() -> Result<Vec<ModelInfo>, std::io::Error> {
        let file_contents = include_str!("../../models.json");
        let response: ModelsResponse = serde_json::from_str(file_contents)?;
        Ok(response.models)
    }

    /// Attempt to satisfy the refresh from the cache when it matches the provider and TTL.
    async fn try_load_cache(&self) -> bool {
        let _timer =
            trill_otel::start_global_timer("codex.remote_models.load_cache.duration_ms", &[]);
        let cache = match self.cache_manager.load_fresh().await {
            Some(cache) => cache,
            None => return false,
        };
        let models = cache.models.clone();
        *self.etag.write().await = cache.etag.clone();
        self.apply_remote_models(models.clone()).await;
        true
    }

    /// Merge remote model metadata into picker-ready presets, preserving existing entries.
    fn build_available_models(&self, mut remote_models: Vec<ModelInfo>) -> Vec<ModelPreset> {
        remote_models.sort_by(|a, b| a.priority.cmp(&b.priority));

        let remote_presets: Vec<ModelPreset> = remote_models.into_iter().map(Into::into).collect();
        let existing_presets = self.local_models.clone();
        let mut merged_presets = ModelPreset::merge(remote_presets, existing_presets);
        let chatgpt_mode = matches!(
            self.auth_manager.get_internal_auth_mode(),
            Some(AuthMode::Chatgpt)
        );
        merged_presets = ModelPreset::filter_by_auth(merged_presets, chatgpt_mode);

        for preset in &mut merged_presets {
            preset.is_default = false;
        }
        if let Some(default) = merged_presets
            .iter_mut()
            .find(|preset| preset.show_in_picker)
        {
            default.is_default = true;
        } else if let Some(default) = merged_presets.first_mut() {
            default.is_default = true;
        }

        merged_presets
    }

    async fn get_remote_models(&self, config: &Config) -> Vec<ModelInfo> {
        if config.features.enabled(Feature::RemoteModels) {
            self.remote_models.read().await.clone()
        } else {
            Vec::new()
        }
    }

    fn try_get_remote_models(&self, config: &Config) -> Result<Vec<ModelInfo>, TryLockError> {
        if config.features.enabled(Feature::RemoteModels) {
            Ok(self.remote_models.try_read()?.clone())
        } else {
            Ok(Vec::new())
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Construct a manager with a specific provider for testing.
    pub fn with_provider(
        trill_home: PathBuf,
        auth_manager: Arc<AuthManager>,
        provider: ModelProviderInfo,
    ) -> Self {
        let cache_path = trill_home.join(MODEL_CACHE_FILE);
        let cache_manager = ModelsCacheManager::new(cache_path, DEFAULT_MODEL_CACHE_TTL);
        Self {
            local_models: builtin_model_presets(auth_manager.get_internal_auth_mode()),
            remote_models: RwLock::new(Self::load_remote_models_from_file().unwrap_or_default()),
            auth_manager,
            etag: RwLock::new(None),
            cache_manager,
            provider,
            model_provider_id: "openai".to_string(),
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Get model identifier without consulting remote state or cache.
    pub fn get_model_offline(model: Option<&str>) -> String {
        if let Some(model) = model {
            return model.to_string();
        }
        let presets = builtin_model_presets(None);
        presets
            .iter()
            .find(|preset| preset.show_in_picker)
            .or_else(|| presets.first())
            .map(|preset| preset.model.clone())
            .unwrap_or_default()
    }

    #[cfg(any(test, feature = "test-support"))]
    /// Build `ModelInfo` without consulting remote state or cache.
    pub fn construct_model_info_offline(model: &str, config: &Config) -> ModelInfo {
        model_info::with_config_overrides(model_info::find_model_info_for_slug(model), config)
    }
}

/// Convert a client version string to a whole version string (e.g. "1.2.3-alpha.4" -> "1.2.3")
fn format_client_version_to_whole() -> String {
    format!(
        "{}.{}.{}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CodexAuth;
    use crate::auth::AuthCredentialsStoreMode;
    use crate::config::ConfigBuilder;
    use crate::features::Feature;
    use crate::model_provider_info::WireApi;
    use chrono::Utc;
    use trill_protocol::openai_models::ModelsResponse;
    use core_test_support::responses::mount_models_once;
    use pretty_assertions::assert_eq;
    use serde_json::json;
    use tempfile::tempdir;
    use wiremock::MockServer;

    fn remote_model(slug: &str, display: &str, priority: i32) -> ModelInfo {
        remote_model_with_visibility(slug, display, priority, "list")
    }

    fn remote_model_with_visibility(
        slug: &str,
        display: &str,
        priority: i32,
        visibility: &str,
    ) -> ModelInfo {
        serde_json::from_value(json!({
            "slug": slug,
            "display_name": display,
            "description": format!("{display} desc"),
            "default_reasoning_level": "medium",
            "supported_reasoning_levels": [{"effort": "low", "description": "low"}, {"effort": "medium", "description": "medium"}],
            "shell_type": "shell_command",
            "visibility": visibility,
            "minimal_client_version": [0, 1, 0],
            "supported_in_api": true,
            "priority": priority,
            "upgrade": null,
            "base_instructions": "base instructions",
            "supports_reasoning_summaries": false,
            "support_verbosity": false,
            "default_verbosity": null,
            "apply_patch_tool_type": null,
            "truncation_policy": {"mode": "bytes", "limit": 10_000},
            "supports_parallel_tool_calls": false,
            "context_window": 272_000,
            "experimental_supported_tools": [],
        }))
        .expect("valid model")
    }

    fn assert_models_contain(actual: &[ModelInfo], expected: &[ModelInfo]) {
        for model in expected {
            assert!(
                actual.iter().any(|candidate| candidate.slug == model.slug),
                "expected model {} in cached list",
                model.slug
            );
        }
    }

    fn provider_for(base_url: String) -> ModelProviderInfo {
        ModelProviderInfo {
            name: "mock".into(),
            base_url: Some(base_url),
            env_key: None,
            env_key_instructions: None,
            experimental_bearer_token: None,
            wire_api: WireApi::Responses,
            query_params: None,
            http_headers: None,
            env_http_headers: None,
            request_max_retries: Some(0),
            stream_max_retries: Some(0),
            stream_idle_timeout_ms: Some(5_000),
            requires_openai_auth: false,
            supports_websockets: false,
        }
    }

    #[tokio::test]
    async fn refresh_available_models_sorts_by_priority() {
        let server = MockServer::start().await;
        let remote_models = vec![
            remote_model("priority-low", "Low", 1),
            remote_model("priority-high", "High", 0),
        ];
        let models_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: remote_models.clone(),
            },
        )
        .await;

        let trill_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .trill_home(trill_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let provider = provider_for(server.uri());
        let manager =
            ModelsManager::with_provider(trill_home.path().to_path_buf(), auth_manager, provider);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("refresh succeeds");
        let cached_remote = manager.get_remote_models(&config).await;
        assert_models_contain(&cached_remote, &remote_models);

        let available = manager
            .list_models(&config, RefreshStrategy::OnlineIfUncached)
            .await;
        let high_idx = available
            .iter()
            .position(|model| model.model == "priority-high")
            .expect("priority-high should be listed");
        let low_idx = available
            .iter()
            .position(|model| model.model == "priority-low")
            .expect("priority-low should be listed");
        assert!(
            high_idx < low_idx,
            "higher priority should be listed before lower priority"
        );
        assert_eq!(
            models_mock.requests().len(),
            1,
            "expected a single /models request"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_uses_cache_when_fresh() {
        let server = MockServer::start().await;
        let remote_models = vec![remote_model("cached", "Cached", 5)];
        let models_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: remote_models.clone(),
            },
        )
        .await;

        let trill_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .trill_home(trill_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager = Arc::new(AuthManager::new(
            trill_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let provider = provider_for(server.uri());
        let manager =
            ModelsManager::with_provider(trill_home.path().to_path_buf(), auth_manager, provider);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("first refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &remote_models);

        // Second call should read from cache and avoid the network.
        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("cached refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &remote_models);
        assert_eq!(
            models_mock.requests().len(),
            1,
            "cache hit should avoid a second /models request"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_refetches_when_cache_stale() {
        let server = MockServer::start().await;
        let initial_models = vec![remote_model("stale", "Stale", 1)];
        let initial_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: initial_models.clone(),
            },
        )
        .await;

        let trill_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .trill_home(trill_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager = Arc::new(AuthManager::new(
            trill_home.path().to_path_buf(),
            false,
            AuthCredentialsStoreMode::File,
        ));
        let provider = provider_for(server.uri());
        let manager =
            ModelsManager::with_provider(trill_home.path().to_path_buf(), auth_manager, provider);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("initial refresh succeeds");

        // Rewrite cache with an old timestamp so it is treated as stale.
        manager
            .cache_manager
            .manipulate_cache_for_test(|fetched_at| {
                *fetched_at = Utc::now() - chrono::Duration::hours(1);
            })
            .await
            .expect("cache manipulation succeeds");

        let updated_models = vec![remote_model("fresh", "Fresh", 9)];
        server.reset().await;
        let refreshed_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: updated_models.clone(),
            },
        )
        .await;

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("second refresh succeeds");
        assert_models_contain(&manager.get_remote_models(&config).await, &updated_models);
        assert_eq!(
            initial_mock.requests().len(),
            1,
            "initial refresh should only hit /models once"
        );
        assert_eq!(
            refreshed_mock.requests().len(),
            1,
            "stale cache refresh should fetch /models once"
        );
    }

    #[tokio::test]
    async fn refresh_available_models_drops_removed_remote_models() {
        let server = MockServer::start().await;
        let initial_models = vec![remote_model("remote-old", "Remote Old", 1)];
        let initial_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: initial_models,
            },
        )
        .await;

        let trill_home = tempdir().expect("temp dir");
        let mut config = ConfigBuilder::default()
            .trill_home(trill_home.path().to_path_buf())
            .build()
            .await
            .expect("load default test config");
        config.features.enable(Feature::RemoteModels);
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::create_dummy_chatgpt_auth_for_testing());
        let provider = provider_for(server.uri());
        let mut manager =
            ModelsManager::with_provider(trill_home.path().to_path_buf(), auth_manager, provider);
        manager.cache_manager.set_ttl(Duration::ZERO);

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("initial refresh succeeds");

        server.reset().await;
        let refreshed_models = vec![remote_model("remote-new", "Remote New", 1)];
        let refreshed_mock = mount_models_once(
            &server,
            ModelsResponse {
                models: refreshed_models,
            },
        )
        .await;

        manager
            .refresh_available_models(&config, RefreshStrategy::OnlineIfUncached)
            .await
            .expect("second refresh succeeds");

        let available = manager
            .try_list_models(&config)
            .expect("models should be available");
        assert!(
            available.iter().any(|preset| preset.model == "remote-new"),
            "new remote model should be listed"
        );
        assert!(
            !available.iter().any(|preset| preset.model == "remote-old"),
            "removed remote model should not be listed"
        );
        assert_eq!(
            initial_mock.requests().len(),
            1,
            "initial refresh should only hit /models once"
        );
        assert_eq!(
            refreshed_mock.requests().len(),
            1,
            "second refresh should only hit /models once"
        );
    }

    #[test]
    fn build_available_models_picks_default_after_hiding_hidden_models() {
        let trill_home = tempdir().expect("temp dir");
        let auth_manager =
            AuthManager::from_auth_for_testing(CodexAuth::from_api_key("Test API Key"));
        let provider = provider_for("http://example.test".to_string());
        let mut manager =
            ModelsManager::with_provider(trill_home.path().to_path_buf(), auth_manager, provider);
        manager.local_models = Vec::new();

        let hidden_model = remote_model_with_visibility("hidden", "Hidden", 0, "hide");
        let visible_model = remote_model_with_visibility("visible", "Visible", 1, "list");

        let expected_hidden = ModelPreset::from(hidden_model.clone());
        let mut expected_visible = ModelPreset::from(visible_model.clone());
        expected_visible.is_default = true;

        let available = manager.build_available_models(vec![hidden_model, visible_model]);

        assert_eq!(available, vec![expected_hidden, expected_visible]);
    }

    #[test]
    fn bundled_models_json_roundtrips() {
        let file_contents = include_str!("../../models.json");
        let response: ModelsResponse =
            serde_json::from_str(file_contents).expect("bundled models.json should deserialize");

        let serialized =
            serde_json::to_string(&response).expect("bundled models.json should serialize");
        let roundtripped: ModelsResponse =
            serde_json::from_str(&serialized).expect("serialized models.json should deserialize");

        assert_eq!(
            response, roundtripped,
            "bundled models.json should round trip through serde"
        );
        assert!(
            !response.models.is_empty(),
            "bundled models.json should contain at least one model"
        );
    }
}
