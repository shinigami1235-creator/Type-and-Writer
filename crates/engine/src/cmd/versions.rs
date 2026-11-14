//! Version history commands (Type and Writer): list the copies every save keeps
//! ([`crate::history`]), open one beside the document, compare one with the document as it is
//! now (the differences shown as tracked changes in a new window), or bring one back in place of
//! the document (one Undo takes it back).

use serde_json::{Value, json};

use crate::{CmdError, CmdResult, CommandSpec, Session, p};

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("file.versionHistory", "Version History", "File › Info", history)
            .params(r#"{"quiet"?: bool (list only, no window)}"#)
            .pure(),
        CommandSpec::new("file.openVersion", "Open Version", "File › Info › Version History", open_version).params(r#"{"id": string}"#).pure(),
        CommandSpec::new("file.compareVersion", "Compare with Version", "File › Info › Version History", compare_version)
            .params(r#"{"id": string}"#)
            .pure(),
        CommandSpec::new("file.signAsPdf", "Sign as PDF", "Insert › Text", sign_as_pdf)
            .params(r#"{"path"?: string (where the PDF goes; default: beside the document, same name)}"#)
            .pure(),
        CommandSpec::new("file.restoreVersion", "Restore Version", "File › Info › Version History", restore_version)
            .params(r#"{"id": string}"#)
            .pure(),
    ]
}

fn saved_path(s: &Session) -> Result<std::path::PathBuf, CmdError> {
    if crate::history::root().is_none() {
        return Err(CmdError::Failed("version history is off in this program".into()));
    }
    s.path.clone().ok_or_else(|| CmdError::Failed("save the document first: versions are kept from its first save on".into()))
}

fn version_doc(s: &Session, v: &Value) -> Result<(wordcraft_doc::Document, String), CmdError> {
    let path = saved_path(s)?;
    let id = p::str(v, "id").ok_or_else(|| CmdError::Params("`id` of a version (from file.versionHistory) is required".into()))?;
    let file = crate::history::file(&path, id).ok_or_else(|| CmdError::Params(format!("no version `{id}` of this document")))?;
    let doc = crate::io::open_path(&file).map_err(CmdError::Failed)?;
    let when = crate::history::list(&path).into_iter().find(|x| x.id == id).map(|x| x.saved_at).unwrap_or_else(|| id.to_string());
    Ok((doc, when))
}

fn stem(s: &Session) -> String {
    s.path.as_deref().and_then(|p| p.file_stem()).map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| "Document".into())
}

fn history(s: &mut Session, v: &Value) -> CmdResult {
    let versions = match &s.path {
        Some(path) if crate::history::root().is_some() => crate::history::list(path),
        _ => Vec::new(),
    };
    let list = json!(versions);
    if !p::bool(v, "quiet").unwrap_or(false) {
        s.push_ui_request(json!({"open": "versionHistory"}));
    }
    Ok(json!({"versions": list, "saved": s.path.is_some(), "on": crate::history::root().is_some()}))
}

fn open_version(s: &mut Session, v: &Value) -> CmdResult {
    let (doc, when) = version_doc(s, v)?;
    let name = format!("{} (saved {when})", stem(s));
    let id = s.add_document(doc);
    s.file_name = Some(name.clone());
    Ok(json!({"id": id, "name": name}))
}

fn compare_version(s: &mut Session, v: &Value) -> CmdResult {
    let (old, when) = version_doc(s, v)?;
    let current = s.doc.clone();
    let name = format!("{}: changes since {when}", stem(s));
    // The comparison writes the current document to a scratch file the compare reads.
    let scratch = crate::history::scratch(&saved_path(s)?, "compare-current.docx").map_err(CmdError::Failed)?;
    crate::io::save_path_with(&scratch, &current, None, &Default::default()).map_err(CmdError::Failed)?;
    let id = s.add_document(old);
    s.file_name = Some(name.clone());
    let r = s.run("review.compare", &json!({"path": scratch.to_string_lossy(), "author": "Changes"}));
    crate::history::remove_scratch(&scratch);
    let r = r?;
    s.dirty = false;
    Ok(json!({"id": id, "name": name, "compare": r}))
}

fn restore_version(s: &mut Session, v: &Value) -> CmdResult {
    let (doc, when) = version_doc(s, v)?;
    // As file.versions: a restore must not go around a protection, nor bring an old one back.
    crate::cmd::protect::check_version_restore(s)?;
    s.checkpoint("Restore Version");
    let protection = s.doc.settings.protection.clone();
    let protect_hash = s.doc.settings.protect_hash.clone();
    let protect_formatting = s.doc.settings.protect_formatting;
    s.doc = doc;
    s.doc.ensure_nonempty();
    s.doc.settings.protection = protection;
    s.doc.settings.protect_hash = protect_hash;
    s.doc.settings.protect_formatting = protect_formatting;
    s.note_body_replaced();
    s.math = None;
    s.touch();
    s.clamp_selection();
    Ok(json!({"restored": when}))
}

/// Sign as PDF (Type and Writer): the document as a PDF beside it (same name), handed to the PDF
/// reader the app is set to sign with (the Cygnus Solutions reader, or the computer's PDF app).
fn sign_as_pdf(s: &mut Session, v: &Value) -> CmdResult {
    let target = match p::str(v, "path") {
        Some(p) => std::path::PathBuf::from(p),
        None => match &s.path {
            Some(doc) => doc.with_extension("pdf"),
            None => {
                let dir = crate::history::temp_dir().map_err(CmdError::Failed)?;
                dir.join(format!("{}.pdf", s.file_name.clone().unwrap_or_else(|| "Document".into())))
            }
        },
    };
    if s.path.as_deref() == Some(target.as_path()) {
        return Err(CmdError::Params("the PDF would replace the document itself; give another `path`".into()));
    }
    s.run("file.exportPdf", &json!({"path": target.to_string_lossy()}))?;
    s.push_ui_request(json!({"openExternal": target.to_string_lossy(), "purpose": "sign"}));
    Ok(json!({"pdf": target.to_string_lossy()}))
}
