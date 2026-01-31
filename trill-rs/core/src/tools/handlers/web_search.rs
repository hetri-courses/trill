use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;

use crate::function_tool::FunctionCallError;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::handlers::parse_arguments;
use crate::tools::registry::ToolHandler;
use crate::tools::registry::ToolKind;

/// Default SearXNG URL - can be overridden via config
const DEFAULT_SEARXNG_URL: &str = "http://192.168.0.137:8080";
const DEFAULT_TIMEOUT_SECS: u64 = 30;
const DEFAULT_NUM_RESULTS: usize = 10;

pub struct WebSearchHandler {
    client: Client,
    searxng_url: String,
}

impl Default for WebSearchHandler {
    fn default() -> Self {
        Self::new(DEFAULT_SEARXNG_URL.to_string())
    }
}

impl WebSearchHandler {
    pub fn new(searxng_url: String) -> Self {
        let client = Client::builder()
            .timeout(Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .expect("Failed to create HTTP client");

        Self {
            client,
            searxng_url,
        }
    }
}

/// Arguments for web_search function call
#[derive(Debug, Deserialize)]
struct WebSearchArgs {
    /// Search query string
    #[serde(default)]
    query: Option<String>,

    /// Multiple search queries
    #[serde(default)]
    queries: Option<Vec<String>>,

    /// URL to open (for OpenPage action)
    #[serde(default)]
    url: Option<String>,

    /// Pattern to find in page (for FindInPage action)
    #[serde(default)]
    pattern: Option<String>,

    /// Action type hint (auto-detected if not provided)
    #[serde(default)]
    action: Option<String>,
}

/// SearXNG search result
#[derive(Debug, Deserialize)]
struct SearxngResult {
    title: Option<String>,
    url: Option<String>,
    content: Option<String>,
    engine: Option<String>,
    #[serde(default)]
    engines: Vec<String>,
    score: Option<f64>,
    category: Option<String>,
    #[serde(rename = "publishedDate")]
    published_date: Option<String>,
}

/// SearXNG API response
#[derive(Debug, Deserialize)]
struct SearxngResponse {
    results: Vec<SearxngResult>,
    #[serde(default)]
    query: String,
}

/// Web search result item returned to model
#[derive(Debug, Serialize)]
struct WebSearchResultItem {
    title: String,
    url: String,
    snippet: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    engine: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    engines: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    score: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    published_date: Option<String>,
}

/// WebSearchCall response format
#[derive(Debug, Serialize)]
struct WebSearchCallResponse {
    status: String,
    action: WebSearchActionResponse,
    results: Option<Vec<WebSearchResultItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    page_content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    matches: Option<Vec<String>>,
}

/// WebSearchAction in response
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum WebSearchActionResponse {
    Search {
        #[serde(skip_serializing_if = "Option::is_none")]
        query: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        queries: Option<Vec<String>>,
    },
    OpenPage {
        url: String,
    },
    FindInPage {
        url: String,
        pattern: String,
    },
}

impl WebSearchHandler {
    /// Determine action type from arguments
    fn determine_action(args: &WebSearchArgs) -> Result<WebSearchActionType, FunctionCallError> {
        // If action is explicitly specified
        if let Some(action) = &args.action {
            return match action.to_lowercase().as_str() {
                "search" => Ok(WebSearchActionType::Search),
                "open_page" | "openpage" => Ok(WebSearchActionType::OpenPage),
                "find_in_page" | "findinpage" => Ok(WebSearchActionType::FindInPage),
                _ => Err(FunctionCallError::RespondToModel(
                    format!("Unknown action type: {}", action)
                )),
            };
        }

        // Auto-detect based on provided fields
        if args.url.is_some() && args.pattern.is_some() {
            Ok(WebSearchActionType::FindInPage)
        } else if args.url.is_some() {
            Ok(WebSearchActionType::OpenPage)
        } else if args.query.is_some() || args.queries.is_some() {
            Ok(WebSearchActionType::Search)
        } else {
            Err(FunctionCallError::RespondToModel(
                "Unable to determine action type: provide query, url, or url+pattern".to_string()
            ))
        }
    }

    /// Execute a search via SearXNG
    async fn execute_search(&self, query: &str) -> Result<Vec<WebSearchResultItem>, FunctionCallError> {
        let search_url = format!("{}/search", self.searxng_url);

        let response = self.client
            .get(&search_url)
            .query(&[("q", query), ("format", "json")])
            .send()
            .await
            .map_err(|e| FunctionCallError::RespondToModel(
                format!("SearXNG request failed: {}", e)
            ))?;

        if !response.status().is_success() {
            return Err(FunctionCallError::RespondToModel(
                format!("SearXNG returned error status: {}", response.status())
            ));
        }

        let searxng_response: SearxngResponse = response.json().await
            .map_err(|e| FunctionCallError::RespondToModel(
                format!("Failed to parse SearXNG response: {}", e)
            ))?;

        let results: Vec<WebSearchResultItem> = searxng_response.results
            .into_iter()
            .take(DEFAULT_NUM_RESULTS)
            .map(|r| WebSearchResultItem {
                title: r.title.unwrap_or_default(),
                url: r.url.unwrap_or_default(),
                snippet: r.content.unwrap_or_default(),
                engine: r.engine,
                engines: if r.engines.is_empty() { None } else { Some(r.engines) },
                score: r.score,
                category: r.category,
                published_date: r.published_date,
            })
            .collect();

        Ok(results)
    }

    /// Fetch a page and return its content
    async fn execute_open_page(&self, url: &str) -> Result<String, FunctionCallError> {
        let response = self.client
            .get(url)
            .send()
            .await
            .map_err(|e| FunctionCallError::RespondToModel(
                format!("Failed to fetch page: {}", e)
            ))?;

        if !response.status().is_success() {
            return Err(FunctionCallError::RespondToModel(
                format!("Page returned error status: {}", response.status())
            ));
        }

        let content = response.text().await
            .map_err(|e| FunctionCallError::RespondToModel(
                format!("Failed to read page content: {}", e)
            ))?;

        // Truncate very long content
        const MAX_CONTENT_LENGTH: usize = 50000;
        if content.len() > MAX_CONTENT_LENGTH {
            Ok(format!("{}...\n[Content truncated at {} characters]",
                &content[..MAX_CONTENT_LENGTH], MAX_CONTENT_LENGTH))
        } else {
            Ok(content)
        }
    }

    /// Find pattern in page content
    async fn execute_find_in_page(&self, url: &str, pattern: &str) -> Result<Vec<String>, FunctionCallError> {
        let content = self.execute_open_page(url).await?;

        let mut matches = Vec::new();
        let pattern_lower = pattern.to_lowercase();

        for (i, line) in content.lines().enumerate() {
            if line.to_lowercase().contains(&pattern_lower) {
                // Include line number and context
                matches.push(format!("Line {}: {}", i + 1, line.trim()));
            }
        }

        // Limit number of matches returned
        const MAX_MATCHES: usize = 50;
        if matches.len() > MAX_MATCHES {
            matches.truncate(MAX_MATCHES);
            matches.push(format!("[... and more matches, showing first {}]", MAX_MATCHES));
        }

        Ok(matches)
    }
}

#[derive(Debug)]
enum WebSearchActionType {
    Search,
    OpenPage,
    FindInPage,
}

#[async_trait]
impl ToolHandler for WebSearchHandler {
    fn kind(&self) -> ToolKind {
        ToolKind::Function
    }

    async fn handle(&self, invocation: ToolInvocation) -> Result<ToolOutput, FunctionCallError> {
        let ToolInvocation {
            payload,
            ..
        } = invocation;

        let arguments = match payload {
            ToolPayload::Function { arguments } => arguments,
            _ => {
                return Err(FunctionCallError::RespondToModel(
                    "web_search handler received unsupported payload".to_string(),
                ));
            }
        };

        let args: WebSearchArgs = parse_arguments(&arguments)?;
        let action_type = Self::determine_action(&args)?;

        let response = match action_type {
            WebSearchActionType::Search => {
                let queries: Vec<String> = if let Some(queries) = args.queries {
                    queries
                } else if let Some(query) = args.query {
                    vec![query]
                } else {
                    return Err(FunctionCallError::RespondToModel(
                        "Search action requires query or queries field".to_string()
                    ));
                };

                // Execute all queries and combine results
                let mut all_results = Vec::new();
                for query in &queries {
                    match self.execute_search(query).await {
                        Ok(results) => all_results.extend(results),
                        Err(e) => {
                            // Log error but continue with other queries
                            tracing::warn!("Search query '{}' failed: {:?}", query, e);
                        }
                    }
                }

                let action = WebSearchActionResponse::Search {
                    query: if queries.len() == 1 { Some(queries[0].clone()) } else { None },
                    queries: if queries.len() > 1 { Some(queries) } else { None },
                };

                WebSearchCallResponse {
                    status: "completed".to_string(),
                    action,
                    results: Some(all_results),
                    page_content: None,
                    matches: None,
                }
            }

            WebSearchActionType::OpenPage => {
                let url = args.url.ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "OpenPage action requires url field".to_string()
                    )
                })?;

                let content = self.execute_open_page(&url).await?;

                WebSearchCallResponse {
                    status: "completed".to_string(),
                    action: WebSearchActionResponse::OpenPage { url: url.clone() },
                    results: None,
                    page_content: Some(content),
                    matches: None,
                }
            }

            WebSearchActionType::FindInPage => {
                let url = args.url.ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "FindInPage action requires url field".to_string()
                    )
                })?;
                let pattern = args.pattern.ok_or_else(|| {
                    FunctionCallError::RespondToModel(
                        "FindInPage action requires pattern field".to_string()
                    )
                })?;

                let matches = self.execute_find_in_page(&url, &pattern).await?;

                WebSearchCallResponse {
                    status: "completed".to_string(),
                    action: WebSearchActionResponse::FindInPage {
                        url: url.clone(),
                        pattern: pattern.clone()
                    },
                    results: None,
                    page_content: None,
                    matches: Some(matches),
                }
            }
        };

        let content = serde_json::to_string(&response)
            .map_err(|e| FunctionCallError::RespondToModel(
                format!("Failed to serialize response: {}", e)
            ))?;

        Ok(ToolOutput::Function {
            content,
            content_items: None,
            success: Some(true),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_determine_action_search() {
        let args = WebSearchArgs {
            query: Some("test query".to_string()),
            queries: None,
            url: None,
            pattern: None,
            action: None,
        };

        let action = WebSearchHandler::determine_action(&args).unwrap();
        assert!(matches!(action, WebSearchActionType::Search));
    }

    #[test]
    fn test_determine_action_open_page() {
        let args = WebSearchArgs {
            query: None,
            queries: None,
            url: Some("https://example.com".to_string()),
            pattern: None,
            action: None,
        };

        let action = WebSearchHandler::determine_action(&args).unwrap();
        assert!(matches!(action, WebSearchActionType::OpenPage));
    }

    #[test]
    fn test_determine_action_find_in_page() {
        let args = WebSearchArgs {
            query: None,
            queries: None,
            url: Some("https://example.com".to_string()),
            pattern: Some("search term".to_string()),
            action: None,
        };

        let action = WebSearchHandler::determine_action(&args).unwrap();
        assert!(matches!(action, WebSearchActionType::FindInPage));
    }

    #[test]
    fn test_determine_action_explicit() {
        let args = WebSearchArgs {
            query: Some("test".to_string()),
            queries: None,
            url: Some("https://example.com".to_string()),
            pattern: Some("term".to_string()),
            action: Some("search".to_string()),
        };

        // Even with all fields present, explicit action takes precedence
        let action = WebSearchHandler::determine_action(&args).unwrap();
        assert!(matches!(action, WebSearchActionType::Search));
    }
}
