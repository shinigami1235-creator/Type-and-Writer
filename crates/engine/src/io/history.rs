//! Version history (Type and Writer): every save of a document keeps a copy of the file as it
//! was written, in the app's own folder, so an earlier version can be opened, compared with the
//! current one or brought back. Each document has a folder named after a hash of its path; the
//! newest [`KEEP`] copies are kept.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde::Serialize;

/// Copies kept per document.
pub const KEEP: usize = 50;
/// Files larger than this aren't copied (big picture-heavy files would fill the disk).
pub const MAX_BYTES: u64 = 200 << 20;

static ROOT: OnceLock<PathBuf> = OnceLock::new();

/// Turn version history on, keeping copies under `root` (the desktop app and the CLI call this
/// with their settings folder). Without it nothing is kept.
pub fn set_root(root: PathBuf) {
    let _ = ROOT.set(root);
}

/// The history folder, unless history is off or an agent's file access is confined to its
/// roots (`wordcraft-cli mcp --automation-…-root`): a confined session never writes outside them.
pub fn root() -> Option<&'static Path> {
    if crate::file_access::current().is_some() {
        return None;
    }
    ROOT.get().map(PathBuf::as_path)
}

/// A scratch file in `path`'s history folder (the folder is made).
pub fn scratch(path: &Path, name: &str) -> Result<PathBuf, String> {
    let dir = folder(path).ok_or("version history is off")?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join(name))
}

/// Remove a scratch file made by [`scratch`].
pub fn remove_scratch(file: &Path) {
    let _ = std::fs::remove_file(file);
}

/// A folder for files made for another program (Sign as PDF of an unsaved document).
pub fn temp_dir() -> Result<PathBuf, String> {
    let dir = std::env::temp_dir().join("Type and Writer");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir)
}

/// One kept version.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Version {
    /// Its file name in the document's folder: what the commands take.
    pub id: String,
    /// When it was saved, local time, `2026-11-14 19:20:05`.
    pub saved_at: String,
    pub bytes: u64,
}

/// FNV-1a: a stable folder name for a path (case folded, so `Report.docx` and `report.docx` on
/// Windows share one).
fn key(path: &Path) -> String {
    let s = path.to_string_lossy().to_lowercase();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

/// The folder holding `path`'s versions.
pub fn folder(path: &Path) -> Option<PathBuf> {
    Some(root()?.join(key(path)))
}

fn saved_at(id: &str) -> String {
    // `2026-11-14_19-20-05.docx` (or with a `_2` suffix when two saves share a second).
    let stem = id.split('.').next().unwrap_or(id);
    let mut parts = stem.splitn(2, '_');
    let date = parts.next().unwrap_or("");
    let time = parts.next().unwrap_or("").get(..8).unwrap_or("").replace('-', ":");
    format!("{date} {time}").trim().to_string()
}

/// The versions kept of `path`, newest first.
pub fn list(path: &Path) -> Vec<Version> {
    let Some(dir) = folder(path) else { return Vec::new() };
    let Ok(rd) = std::fs::read_dir(&dir) else { return Vec::new() };
    let mut v: Vec<Version> = rd
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            let md = e.metadata().ok()?;
            (md.is_file() && name.as_bytes().first().is_some_and(u8::is_ascii_digit)).then(|| Version {
                saved_at: saved_at(&name),
                id: name,
                bytes: md.len(),
            })
        })
        .collect();
    v.sort_by(|a, b| b.id.cmp(&a.id));
    v
}

/// The file of version `id` of `path`, if it is one of its versions (an id can't reach outside
/// the document's folder).
pub fn file(path: &Path, id: &str) -> Option<PathBuf> {
    if id.is_empty() || id.contains(['/', '\\']) || id.contains("..") {
        return None;
    }
    let f = folder(path)?.join(id);
    f.is_file().then_some(f)
}

/// Keep a copy of the file just saved at `path`. Skipped when it's the same as the newest copy,
/// too big, or history is off. The oldest copies past [`KEEP`] go.
pub fn record(path: &Path) -> Result<Option<Version>, String> {
    let Some(dir) = folder(path) else { return Ok(None) };
    let md = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if !md.is_file() || md.len() > MAX_BYTES {
        return Ok(None);
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    // Which file the folder is for, for anyone looking in it.
    let _ = std::fs::write(dir.join("source.txt"), path.to_string_lossy().as_bytes());
    let existing = list(path);
    if let Some(newest) = existing.first()
        && newest.bytes == bytes.len() as u64
        && std::fs::read(dir.join(&newest.id)).is_ok_and(|b| b == bytes)
    {
        return Ok(None);
    }
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("docx").to_ascii_lowercase();
    let stamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let mut id = format!("{stamp}.{ext}");
    let mut n = 2;
    while dir.join(&id).exists() && n < 100 {
        id = format!("{stamp}_{n}.{ext}");
        n += 1;
    }
    std::fs::write(dir.join(&id), &bytes).map_err(|e| e.to_string())?;
    for old in list(path).into_iter().skip(KEEP) {
        let _ = std::fs::remove_file(dir.join(&old.id));
    }
    Ok(Some(Version { saved_at: saved_at(&id), id, bytes: bytes.len() as u64 }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_stay_in_their_folder() {
        let p = Path::new("/docs/report.docx");
        assert!(file(p, "../../etc/passwd").is_none());
        assert!(file(p, "a/b.docx").is_none());
        assert!(file(p, "").is_none());
    }

    #[test]
    fn saved_at_reads_the_name() {
        assert_eq!(saved_at("2026-11-14_19-20-05.docx"), "2026-11-14 19:20:05");
        assert_eq!(saved_at("2026-11-14_19-20-05_2.docx"), "2026-11-14 19:20:05");
    }

    #[test]
    fn records_lists_and_skips_unchanged() {
        let root = std::env::temp_dir().join(format!("taw-history-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // The root can only be set once per process; this test owns it.
        set_root(root.clone());
        let doc = root.join("work").join("note.txt");
        std::fs::create_dir_all(doc.parent().unwrap()).unwrap();
        std::fs::write(&doc, b"one").unwrap();
        assert!(record(&doc).unwrap().is_some());
        assert!(record(&doc).unwrap().is_none(), "unchanged file is not kept twice");
        std::fs::write(&doc, b"two").unwrap();
        assert!(record(&doc).unwrap().is_some());
        let v = list(&doc);
        assert_eq!(v.len(), 2);
        assert_eq!(std::fs::read(file(&doc, &v[0].id).unwrap()).unwrap(), b"two");
        let _ = std::fs::remove_dir_all(&root);
    }
}
