use std::fs;
use std::path::PathBuf;

use tauri::Manager;

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Config {
    #[serde(default)]
    pub last_collection: Option<String>,
    #[serde(default = "default_zotero_dir")]
    pub zotero_data_dir: String,
    /// LiteLLM chat model string, e.g. "gpt-4o" or "ollama/llama3.1".
    #[serde(default = "default_model")]
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

fn default_model() -> String {
    "gpt-4o".to_string()
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
    /// Load config from the app config dir. On first run (no user config), seed
    /// from a bundled `team_config.json` so team members get a working setup
    /// (shared model/gateway/keys) with ZERO configuration. Never errors.
    pub fn load(app: &tauri::AppHandle) -> Config {
        if let Ok(path) = config_path(app) {
            if let Ok(raw) = fs::read_to_string(&path) {
                if let Ok(mut cfg) = serde_json::from_str::<Config>(&raw) {
                    // Self-heal: a saved config with no key (e.g. seeded before the
                    // team key existed) would otherwise stay keyless forever. If the
                    // bundle ships a key, backfill it so the key never "disappears".
                    let blank = cfg.api_key.as_deref().is_none_or(|k| k.trim().is_empty());
                    if blank {
                        if let Some(team) = Self::bundled_team_config(app) {
                            if team.api_key.as_deref().is_some_and(|k| !k.trim().is_empty()) {
                                cfg.api_key = team.api_key;
                                let _ = cfg.save(app);
                            }
                        }
                    }
                    return cfg;
                }
            }
        }
        // First run: seed from bundled team defaults, then persist so the team
        // key isn't re-read from the bundle on every launch.
        if let Some(cfg) = Self::bundled_team_config(app) {
            let _ = cfg.save(app);
            return cfg;
        }
        Config::default()
    }

    /// Read team defaults from `team_config.json` if shipped with the app.
    /// Checked in the bundled resource dir (release) and next to the binary /
    /// project (dev). Missing/blank fields fall back to per-field defaults.
    fn bundled_team_config(app: &tauri::AppHandle) -> Option<Config> {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(dir) = app.path().resource_dir() {
            candidates.push(dir.join("team_config.json"));
        }
        // Dev fallbacks (cwd is usually src-tauri under `tauri dev`).
        candidates.push(PathBuf::from("team_config.json"));
        candidates.push(PathBuf::from("src-tauri/team_config.json"));
        for p in candidates {
            if let Ok(raw) = fs::read_to_string(&p) {
                if let Ok(cfg) = serde_json::from_str::<Config>(&raw) {
                    return Some(cfg);
                }
            }
        }
        None
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
