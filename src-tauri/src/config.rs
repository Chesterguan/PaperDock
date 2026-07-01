use std::fs;
use std::path::PathBuf;

use tauri::Manager;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    pub last_collection: Option<String>,
    pub zotero_data_dir: String,
    /// LiteLLM chat model string, e.g. "gpt-4o" or "ollama/llama3.1".
    pub model: String,
    /// LiteLLM embedding model, e.g. "text-embedding-3-small" or
    /// "ollama/nomic-embed-text". The index cache is keyed by this — changing
    /// it re-embeds (dimensions must stay consistent within an index).
    #[serde(default = "default_embedding")]
    pub embedding: String,
    /// Optional base URL for a self-hosted LiteLLM backend (e.g. an Ollama
    /// server: "http://homeai:11434"). None = use the provider's default.
    #[serde(default)]
    pub api_base: Option<String>,
    /// Optional Qdrant Cloud cluster URL for a SHARED vector index. When set,
    /// embeddings are stored/loaded there (org-wide reuse) instead of a local
    /// pickle. None = local cache.
    #[serde(default)]
    pub qdrant_url: Option<String>,
    /// Qdrant API key (paired with qdrant_url). Never sent to the frontend.
    #[serde(default)]
    pub qdrant_api_key: Option<String>,
    // LiteLLM API key (e.g. OpenAI) entered in the UI. Used only when the
    // matching env var (OPENAI_API_KEY) isn't already set. Never sent to the
    // frontend — get_config exposes a has_api_key bool instead.
    // ponytail: plaintext in the app config dir, fine for a local single-user
    // desktop app; move to the macOS Keychain if that bar rises.
    #[serde(default)]
    pub api_key: Option<String>,
}

fn default_embedding() -> String {
    "text-embedding-3-small".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Config {
            last_collection: None,
            zotero_data_dir: default_zotero_dir(),
            model: "gpt-4o".to_string(),
            embedding: default_embedding(),
            api_base: None,
            qdrant_url: None,
            qdrant_api_key: None,
            api_key: None,
        }
    }
}

impl Config {
    /// Load config from the app config dir. Returns defaults if the file is
    /// missing, unreadable, or malformed (never errors).
    pub fn load(app: &tauri::AppHandle) -> Config {
        let path = match config_path(app) {
            Ok(p) => p,
            Err(_) => return Config::default(),
        };
        let raw = match fs::read_to_string(&path) {
            Ok(r) => r,
            Err(_) => return Config::default(),
        };
        serde_json::from_str(&raw).unwrap_or_else(|_| Config::default())
    }

    /// Persist config as JSON to the app config dir, creating it if needed.
    pub fn save(&self, app: &tauri::AppHandle) -> Result<(), String> {
        let path = config_path(app)?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| format!("Could not create config directory: {e}"))?;
        }
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| format!("Could not serialize config: {e}"))?;
        fs::write(&path, json).map_err(|e| format!("Could not write config: {e}"))
    }
}

fn config_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Could not resolve config directory: {e}"))?;
    Ok(dir.join("config.json"))
}

/// Default Zotero data dir: `~/Zotero`.
pub fn default_zotero_dir() -> String {
    dirs::home_dir()
        .map(|h| h.join("Zotero"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Zotero".to_string())
}
