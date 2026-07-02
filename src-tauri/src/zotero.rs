use std::path::Path;
use std::time::Duration;

use serde_json::Value;

const ROOT: &str = "http://localhost:23119/api";

/// A Zotero library path segment: "users/0" (My Library) or "groups/<id>".
fn base(library: &str) -> String {
    format!("{ROOT}/{library}")
}

#[derive(Clone, serde::Serialize)]
pub struct Collection {
    pub key: String,
    pub name: String,
    pub num_items: u32,
    /// API segment the collection lives under ("users/0" or "groups/<id>").
    pub library: String,
    /// Human name of the library, for the picker ("My Library" / group name).
    pub library_name: String,
}

#[derive(Clone, serde::Serialize)]
pub struct DocRef {
    pub path: String,
    pub zotero_key: String,
    pub citation: String,
}

/// Result of resolving a collection: the papers with a usable PDF, plus the
/// citations of real papers that were skipped because no PDF is on disk.
pub struct CollectionDocs {
    pub docs: Vec<DocRef>,
    pub skipped: Vec<String>,
}

fn client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| format!("Could not create HTTP client: {e}"))
}

/// True if the Zotero local API responds within ~2s.
pub async fn is_running() -> bool {
    let c = match reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    match c.get("http://localhost:23119/api/").send().await {
        Ok(resp) => resp.status().is_success() || resp.status().as_u16() == 404,
        Err(_) => false,
    }
}

/// List the user's libraries: My Library plus any group libraries.
async fn list_libraries(c: &reqwest::Client) -> Vec<(String, String)> {
    let mut libs = vec![("users/0".to_string(), "My Library".to_string())];
    if let Ok(resp) = c.get(format!("{ROOT}/users/0/groups")).send().await {
        if let Ok(body) = resp.json::<Value>().await {
            if let Some(arr) = body.as_array() {
                for g in arr {
                    let id = g.get("id").and_then(Value::as_u64);
                    let name = g
                        .get("data")
                        .and_then(|d| d.get("name"))
                        .and_then(Value::as_str);
                    if let (Some(id), Some(name)) = (id, name) {
                        libs.push((format!("groups/{id}"), name.to_string()));
                    }
                }
            }
        }
    }
    libs
}

/// List top-level collections across every library (My Library + groups).
pub async fn list_collections() -> Result<Vec<Collection>, String> {
    let c = client()?;
    let libs = list_libraries(&c).await;
    let mut out = Vec::new();
    for (library, library_name) in &libs {
        // Synthetic "All items" entry so a library with papers but no
        // collections (or people who don't use collections) is still queryable.
        let total = library_item_count(&c, library).await;
        if total > 0 {
            out.push(Collection {
                key: ALL_ITEMS.to_string(),
                name: "All items".to_string(),
                num_items: total,
                library: library.clone(),
                library_name: library_name.clone(),
            });
        }
        out.extend(collections_in(&c, library, library_name).await);
    }
    Ok(out)
}

/// Sentinel collection key meaning "the whole library's top-level items".
const ALL_ITEMS: &str = "__all__";

/// Count top-level items in a library via the Total-Results header (cheap).
async fn library_item_count(c: &reqwest::Client, library: &str) -> u32 {
    let url = format!("{}/items/top?limit=1", base(library));
    if let Ok(resp) = c.get(&url).send().await {
        if let Some(n) = resp
            .headers()
            .get("Total-Results")
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<u32>().ok())
        {
            return n;
        }
    }
    0
}

/// Collections within one library (empty on any error — a bad group shouldn't
/// sink the whole list).
async fn collections_in(
    c: &reqwest::Client,
    library: &str,
    library_name: &str,
) -> Vec<Collection> {
    let url = format!("{}/collections", base(library));
    let resp = match c.get(&url).send().await {
        Ok(r) if r.status().is_success() => r,
        _ => return Vec::new(),
    };
    let body: Value = match resp.json().await {
        Ok(b) => b,
        Err(_) => return Vec::new(),
    };
    let arr = match body.as_array() {
        Some(a) => a,
        None => return Vec::new(),
    };

    let mut out = Vec::new();
    for item in arr {
        let key = item
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if key.is_empty() {
            continue;
        }
        let name = item
            .get("data")
            .and_then(|d| d.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("(untitled)")
            .to_string();
        let num_items = item
            .get("meta")
            .and_then(|m| m.get("numItems"))
            .and_then(Value::as_u64)
            .unwrap_or(0) as u32;
        out.push(Collection {
            key,
            name,
            num_items,
            library: library.to_string(),
            library_name: library_name.to_string(),
        });
    }
    out
}

/// Resolve every top-level item in a collection to a DocRef, keeping those with
/// an existing PDF on disk and reporting the ones skipped (no PDF).
pub async fn collection_docs(
    library: &str,
    collection_key: &str,
    data_dir: &str,
) -> Result<CollectionDocs, String> {
    let c = client()?;
    let url = if collection_key == ALL_ITEMS {
        format!("{}/items/top", base(library))
    } else {
        format!("{}/collections/{collection_key}/items/top", base(library))
    };
    let resp = c
        .get(&url)
        .send()
        .await
        .map_err(|_| "Zotero is not running.".to_string())?;
    if !resp.status().is_success() {
        return Err("Could not load papers from this collection.".to_string());
    }
    let items: Value = resp
        .json()
        .await
        .map_err(|_| "Could not parse Zotero items.".to_string())?;
    let items = items
        .as_array()
        .ok_or_else(|| "Unexpected Zotero items response.".to_string())?;

    use futures::stream::{self, StreamExt};

    // Resolve each item's PDF concurrently (cap 8) instead of serially —
    // a 100-item collection was 100 sequential /children roundtrips.
    // Sync pass: pull owned (key, citation) for real top-level items.
    let mut candidates: Vec<(String, String)> = Vec::new();
    for item in items {
        let data = match item.get("data") {
            Some(d) => d,
            None => continue,
        };
        let item_key = data
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if item_key.is_empty() {
            continue;
        }
        // Skip attachments/notes appearing at top level.
        let item_type = data
            .get("itemType")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if item_type == "attachment" || item_type == "note" {
            continue;
        }
        candidates.push((item_key, build_citation(data)));
    }

    let resolved: Vec<(String, Option<DocRef>)> = stream::iter(candidates)
        .map(|(item_key, citation)| {
            let c = &c;
            async move {
                let doc = resolve_pdf_path(c, library, &item_key, data_dir)
                    .await
                    .map(|path| DocRef {
                        path,
                        zotero_key: item_key,
                        citation: citation.clone(),
                    });
                (citation, doc)
            }
        })
        .buffered(8)
        .collect()
        .await;

    let mut docs = Vec::new();
    let mut skipped = Vec::new();
    for (citation, doc) in resolved {
        match doc {
            Some(d) => docs.push(d),
            None => skipped.push(citation),
        }
    }
    Ok(CollectionDocs { docs, skipped })
}

/// citation = first creator lastName + " " + year, fallback to title[..40].
fn build_citation(data: &Value) -> String {
    let last_name = data
        .get("creators")
        .and_then(Value::as_array)
        .and_then(|cs| cs.first())
        .and_then(|c| {
            c.get("lastName")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .or_else(|| c.get("name").and_then(Value::as_str))
        })
        .map(|s| s.to_string());

    let year = data
        .get("date")
        .and_then(Value::as_str)
        .and_then(extract_year);

    if let (Some(name), Some(year)) = (last_name.clone(), year.clone()) {
        return format!("{name} {year}");
    }
    if let Some(name) = last_name {
        return name;
    }

    let title = data
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or("Untitled");
    let truncated: String = title.chars().take(40).collect();
    truncated
}

/// Pull a 4-digit year out of a free-form Zotero date string.
fn extract_year(date: &str) -> Option<String> {
    let bytes = date.as_bytes();
    let mut i = 0;
    while i + 4 <= bytes.len() {
        if bytes[i..i + 4].iter().all(|b| b.is_ascii_digit()) {
            return Some(date[i..i + 4].to_string());
        }
        i += 1;
    }
    None
}

/// Find a PDF attachment for an item and resolve its on-disk path if it exists.
async fn resolve_pdf_path(
    c: &reqwest::Client,
    library: &str,
    item_key: &str,
    data_dir: &str,
) -> Option<String> {
    let url = format!("{}/items/{item_key}/children", base(library));
    let resp = c.get(&url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let children: Value = resp.json().await.ok()?;
    let children = children.as_array()?;

    for child in children {
        let data = match child.get("data") {
            Some(d) => d,
            None => continue,
        };
        let content_type = data
            .get("contentType")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if content_type != "application/pdf" {
            continue;
        }
        let link_mode = data
            .get("linkMode")
            .and_then(Value::as_str)
            .unwrap_or_default();

        let candidate = match link_mode {
            "linked_file" => data
                .get("path")
                .and_then(Value::as_str)
                .map(|s| s.to_string()),
            "imported_file" | "imported_url" => {
                let attach_key = data.get("key").and_then(Value::as_str)?;
                let filename = data.get("filename").and_then(Value::as_str)?;
                Some(format!("{data_dir}/storage/{attach_key}/{filename}"))
            }
            _ => None,
        };

        if let Some(path) = candidate {
            if Path::new(&path).exists() {
                return Some(path);
            }
        }
    }
    None
}
