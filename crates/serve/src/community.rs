//! The community list: places and colour schemes people shared, kept in a
//! GitHub repository (one issue per entry, whose 👍 count is its stars; one
//! release file per entry, whose download count is its downloads). The app
//! reads one signed `index.json` from it; search, filters and sorts happen in
//! the page. Nothing is sent anywhere but the downloads GitHub counts.

use rosie_mandlebrot_core::share::Shared;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// The list this app reads unless the settings say otherwise.
pub const DEFAULT_INDEX: &str = "https://raw.githubusercontent.com/QueenRose4444/rosie-mandlebrot-presets/main/index.json";

/// The key the list's index is signed with (minisign). Not the release key:
/// the list's repository can sign its index, not the app.
const PUBLIC_KEY: &str = "RWTCbt7T1jgTWWtqgmS/JpdnfMj1VcGzaj0lu9mNpAoovbFNa+wS8mS0";

/// Largest index read (the list holds codes, not pictures).
const MAX_INDEX: u64 = 32 << 20;
/// Largest entry file read.
const MAX_ENTRY: u64 = 256 << 10;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    pub id: u64,
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub author: String,
    pub created: String,
    pub code: String,
    pub stars: u64,
    pub downloads: u64,
    /// The entry's page (its issue): where to star it.
    pub page: String,
    /// Its release file: what Get downloads.
    pub download: String,
    #[serde(default)]
    pub depth: Option<f64>,
    #[serde(default)]
    pub julia: Option<bool>,
    #[serde(default)]
    pub interest: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub version: u32,
    pub repo: String,
    pub generated: String,
    pub entries: Vec<Entry>,
}

/// The index as the page gets it: where it came from and whether this is a
/// copy kept from earlier because the list could not be reached.
#[derive(Debug, Clone, Serialize)]
pub struct Listing {
    #[serde(flatten)]
    pub index: Index,
    pub source: String,
    pub offline: Option<String>,
}

fn dir(data: &Path) -> PathBuf {
    data.join("community")
}

fn agent(family: ureq::config::IpFamily, secs: u64) -> ureq::Agent {
    ureq::Agent::config_builder().ip_family(family).timeout_global(Some(Duration::from_secs(secs))).build().into()
}

/// IPv4 first, then any: where IPv6 is offered but goes nowhere, trying it
/// first stalled every fetch for about a minute (measured: 57 s, against 0.1 s
/// over IPv4), and the client does not race the two.
fn fetch(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    if !url.starts_with("https://") {
        return Err(format!("{url}: only https addresses"));
    }
    let get = |a: ureq::Agent| -> Result<Vec<u8>, String> {
        let resp = a.get(url).call().map_err(|e| format!("{url}: {e}"))?;
        resp.into_body().with_config().limit(limit).read_to_vec().map_err(|e| format!("{url}: {e}"))
    };
    get(agent(ureq::config::IpFamily::Ipv4Only, 15)).or_else(|_| get(agent(ureq::config::IpFamily::Any, 60)))
}

/// The index's bytes checked against the list's key.
fn verified(bytes: &[u8], sig: &[u8]) -> Result<Index, String> {
    let pk = minisign_verify::PublicKey::from_base64(PUBLIC_KEY).map_err(|e| e.to_string())?;
    let sig = minisign_verify::Signature::decode(std::str::from_utf8(sig).map_err(|_| "the signature is not text")?).map_err(|e| format!("the signature is damaged: {e}"))?;
    pk.verify(bytes, &sig, false).map_err(|_| "the list's signature does not match: it was changed after it was signed".to_string())?;
    let index: Index = serde_json::from_slice(bytes).map_err(|e| format!("the list is damaged: {e}"))?;
    if index.version != 1 {
        return Err(format!("this list is version {}; update the app to read it", index.version));
    }
    Ok(index)
}

/// The list from `url`, checked; when it cannot be reached, the last good copy.
pub fn listing(data: &Path, url: &str) -> Result<Listing, String> {
    let cache = dir(data);
    let fresh = fetch(url, MAX_INDEX).and_then(|b| fetch(&format!("{url}.minisig"), 4096).map(|s| (b, s)));
    match fresh {
        Ok((bytes, sig)) => {
            let index = verified(&bytes, &sig)?;
            let _ = std::fs::create_dir_all(&cache);
            let _ = std::fs::write(cache.join("index.json"), &bytes);
            let _ = std::fs::write(cache.join("index.json.minisig"), &sig);
            Ok(Listing { index, source: url.to_string(), offline: None })
        }
        Err(e) => {
            let bytes = std::fs::read(cache.join("index.json")).map_err(|_| format!("the community list could not be reached: {e}"))?;
            let sig = std::fs::read(cache.join("index.json.minisig")).map_err(|_| format!("the community list could not be reached: {e}"))?;
            Ok(Listing { index: verified(&bytes, &sig)?, source: url.to_string(), offline: Some(e) })
        }
    }
}

/// Download entry `id` (GitHub counts it) and check it is the code the signed
/// index lists; the code, decoded.
pub fn get(data: &Path, url: &str, id: u64) -> Result<Shared, String> {
    let list = listing(data, url)?;
    let e = list.index.entries.iter().find(|e| e.id == id).ok_or("that entry is not on the list")?;
    let file = fetch(&e.download, MAX_ENTRY)?;
    let text: String = String::from_utf8_lossy(&file).chars().filter(|c| !c.is_whitespace()).collect();
    let listed: String = e.code.chars().filter(|c| !c.is_whitespace()).collect();
    if text != listed {
        return Err("the downloaded file is not the code the list vouches for".into());
    }
    Shared::decode(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A copy of the list's first signed index (and its signature): the app
    // must accept exactly these bytes and nothing changed from them.
    const INDEX: &[u8] = include_bytes!("../tests/data/index.json");
    const SIG: &[u8] = include_bytes!("../tests/data/index.json.minisig");

    #[test]
    fn the_signed_index_is_read_and_a_changed_one_refused() {
        let index = verified(INDEX, SIG).expect("the published index verifies");
        assert_eq!(index.entries.len(), 42);
        assert!(index.entries.iter().all(|e| Shared::decode(&e.code).is_ok()), "every listed code decodes");
        let mut changed = INDEX.to_vec();
        let at = changed.windows(9).position(|w| w == b"\"stars\": ").unwrap() + 9;
        changed[at] = b'7';
        assert!(verified(&changed, SIG).unwrap_err().contains("does not match"));
        assert!(verified(INDEX, b"not a signature").is_err());
    }
}
