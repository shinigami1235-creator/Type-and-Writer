//! Opening a file in another program (Type and Writer's Sign as PDF). The PDF reader that signs
//! is the program named in `signer.txt` in the settings folder (or `TYPE_AND_WRITER_SIGNER`),
//! such as the Cygnus Solutions PDF reader; without one, the computer's own app for PDFs.

use std::path::PathBuf;
use std::process::Command;

/// The program set for signing, if there is one and it exists.
pub fn signer() -> Option<PathBuf> {
    let named = std::env::var_os("TYPE_AND_WRITER_SIGNER").map(PathBuf::from).or_else(|| {
        let file = wordcraft_control_key::settings_dir()?.join("signer.txt");
        let text = std::fs::read_to_string(file).ok()?;
        let line = text.lines().map(str::trim).find(|l| !l.is_empty())?.trim_matches('"').to_string();
        Some(PathBuf::from(line))
    })?;
    named.is_file().then_some(named)
}

pub fn open(path: &str, purpose: &str) -> Result<(), String> {
    if purpose == "sign"
        && let Some(prog) = signer()
    {
        return Command::new(&prog).arg(path).spawn().map(|_| ()).map_err(|e| format!("{} didn't start: {e}", prog.display()));
    }
    #[cfg(windows)]
    let r = Command::new("explorer").arg(path).spawn();
    #[cfg(target_os = "macos")]
    let r = Command::new("open").arg(path).spawn();
    #[cfg(not(any(windows, target_os = "macos")))]
    let r = Command::new("xdg-open").arg(path).spawn();
    r.map(|_| ()).map_err(|e| format!("The PDF is saved at {path}, but it couldn't be opened: {e}"))
}
