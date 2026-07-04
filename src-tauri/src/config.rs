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
    /// User's research field (coarse; the only dimension sent with feedback).
    #[serde(default)]
    pub field: String,
    /// Anonymous-feedback opt-in. None = not asked yet, Some(true/false) = decided.
    #[serde(default)]
    pub tele_consent: Option<bool>,
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
            field: String::new(),
            tele_consent: None,
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

    /// Build a shareable LabConfig from the current config. `include_key`
    /// controls whether the LLM api_key travels (labs using personal keys omit).
    pub fn to_lab_config(&self, include_key: bool) -> LabConfig {
        LabConfig {
            lab_name: None,
            model: Some(self.model.clone()),
            embedding: Some(self.embedding.clone()),
            api_base: self.api_base.clone(),
            qdrant_url: self.qdrant_url.clone(),
            qdrant_api_key: self.qdrant_api_key.clone(),
            api_key: if include_key { self.api_key.clone() } else { None },
            default_collection: None,
        }
    }

    /// Merge a lab config into this config: overwrite each shared field only
    /// when the lab file provides it; preserve zotero_data_dir always; set
    /// last_collection from default_collection only if none is set yet.
    pub fn apply_lab_config(&mut self, lab: &LabConfig) {
        if let Some(m) = &lab.model {
            self.model = m.clone();
        }
        if let Some(e) = &lab.embedding {
            self.embedding = e.clone();
        }
        if lab.api_base.is_some() {
            self.api_base = lab.api_base.clone();
        }
        if lab.qdrant_url.is_some() {
            self.qdrant_url = lab.qdrant_url.clone();
        }
        if lab.qdrant_api_key.is_some() {
            self.qdrant_api_key = lab.qdrant_api_key.clone();
        }
        if lab.api_key.is_some() {
            self.api_key = lab.api_key.clone();
        }
        if self.last_collection.is_none() {
            if let Some(dc) = &lab.default_collection {
                self.last_collection = Some(dc.clone());
            }
        }
    }
}

fn config_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_config_dir()
        .map_err(|e| format!("Could not resolve config directory: {e}"))?;
    Ok(dir.join("config.json"))
}

/// The SHARED subset of config an admin distributes to members as a
/// `.paperdock` file. Per-machine fields (zotero_data_dir, personal
/// last_collection) never travel. `api_key` is optional so a lab can require
/// members to use their own.
#[derive(Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct LabConfig {
    #[serde(default)]
    pub lab_name: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub embedding: Option<String>,
    #[serde(default)]
    pub api_base: Option<String>,
    #[serde(default)]
    pub qdrant_url: Option<String>,
    #[serde(default)]
    pub qdrant_api_key: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub default_collection: Option<String>,
}

impl LabConfig {
    pub fn summary_name(&self) -> String {
        self.lab_name
            .clone()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| "your lab".to_string())
    }
}

/// Default Zotero data dir: `~/Zotero`.
pub fn default_zotero_dir() -> String {
    dirs::home_dir()
        .map(|h| h.join("Zotero"))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Zotero".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Config {
        Config {
            last_collection: Some("users/0::PERSONAL".into()),
            zotero_data_dir: "/Users/me/Zotero".into(),
            model: "openai/gpt-oss-120b".into(),
            embedding: "openai/nomic".into(),
            api_base: Some("https://api.ai.it.ufl.edu/v1".into()),
            qdrant_url: Some("https://q.example".into()),
            qdrant_api_key: Some("QKEY".into()),
            api_key: Some("LLMKEY".into()),
            field: String::new(),
            tele_consent: None,
        }
    }

    #[test]
    fn to_lab_config_includes_key_when_asked() {
        let lab = sample().to_lab_config(true);
        assert_eq!(lab.api_key.as_deref(), Some("LLMKEY"));
        assert_eq!(lab.qdrant_api_key.as_deref(), Some("QKEY"));
        assert_eq!(lab.model.as_deref(), Some("openai/gpt-oss-120b"));
        // Per-machine fields never travel.
        assert!(lab.default_collection.is_none());
    }

    #[test]
    fn to_lab_config_omits_key_when_not_asked() {
        let lab = sample().to_lab_config(false);
        assert!(lab.api_key.is_none());
        assert_eq!(lab.qdrant_api_key.as_deref(), Some("QKEY")); // shared, still travels
    }

    #[test]
    fn apply_overwrites_shared_preserves_local() {
        let mut cfg = Config::default(); // zotero_data_dir = default, last_collection None
        let dir_before = cfg.zotero_data_dir.clone();
        let lab = LabConfig {
            lab_name: Some("Smith Lab".into()),
            model: Some("openai/gpt-oss-120b".into()),
            embedding: Some("openai/nomic".into()),
            api_base: Some("https://api.ai.it.ufl.edu/v1".into()),
            qdrant_url: Some("https://q.example".into()),
            qdrant_api_key: Some("QKEY".into()),
            api_key: Some("LLMKEY".into()),
            default_collection: Some("groups/1234567::__all__".into()),
        };
        cfg.apply_lab_config(&lab);
        assert_eq!(cfg.model, "openai/gpt-oss-120b");
        assert_eq!(cfg.qdrant_api_key.as_deref(), Some("QKEY"));
        assert_eq!(cfg.zotero_data_dir, dir_before); // preserved
        assert_eq!(cfg.last_collection.as_deref(), Some("groups/1234567::__all__")); // set: was None
    }

    #[test]
    fn apply_does_not_clobber_existing_collection() {
        let mut cfg = sample(); // last_collection = users/0::PERSONAL
        let lab = LabConfig {
            default_collection: Some("groups/1234567::__all__".into()),
            ..LabConfig::default()
        };
        cfg.apply_lab_config(&lab);
        assert_eq!(cfg.last_collection.as_deref(), Some("users/0::PERSONAL")); // kept
    }
}
