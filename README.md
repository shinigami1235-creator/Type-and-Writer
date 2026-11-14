# Type and Writer

Type and Writer is the Cygnus Solutions word processor for Windows. It looks and works like Microsoft Word, with the same ribbon, tabs and shortcuts, and opens and saves .docx files. It is built on WordCraft, the open-source word processor written in Rust, and adds the features below.

## What it adds to WordCraft

- **Opening PDFs.** File › Open takes a PDF and turns it into an editable document with its fonts, headings, lists, columns, ruled tables and pictures. Running headers, footers and page numbers are left out. Scanned pages and pages in fonts that hide their letters go through OCR. Save writes a .docx beside the PDF and the PDF stays as it was.
- **Clean paste from AI chats.** Text copied from Claude or ChatGPT comes in with real headings, lists, tables, bold and italic instead of `#`, `**` and `|`. Paste Special lists it as "Text from an AI chat (Markdown)".
- **Version history.** Every save keeps a copy, up to 50 per document. File › Info › Version History, or Review › Version History, opens one, compares it with the current document as tracked changes, or restores it. Undo takes a restore back.
- **Dictate.** Home › Dictate types what you say at the cursor. The speech model runs on the laptop with no internet connection.
- **Sign as PDF.** Insert › Sign as PDF saves the document as a PDF beside it and opens it in the PDF reader that signs. Put the reader's program path in `signer.txt` in the settings folder to use the Cygnus Solutions PDF reader, or leave it out to use the default PDF app.
- **Claude extension.** `type-and-writer.mcpb` lets Claude write and edit documents in the open window. Claude opens Type and Writer itself when it isn't running.

## Install

1. Open the newest release on the [Releases page](../../releases).
2. Download `type-and-writer-<version>-windows-x64.msi` and run it. It installs for your user account only and needs no admin rights.
3. To connect Claude, download `type-and-writer.mcpb` and double-click it with the Claude desktop app open, then press Install.

The portable zip has the same program for running without installing.

## Where things are kept

| What | Folder |
|---|---|
| Program | `%LOCALAPPDATA%\Programs\Cygnus Solutions\Type and Writer` |
| Settings, templates, fonts you added | `%APPDATA%\Type and Writer` |
| Version history | `%APPDATA%\Type and Writer\history` |
| OCR models | `ocr` in the program folder |
| Speech engine and model | `whisper` in the program folder |

## Building

The Windows workflow in `.github/workflows/windows.yml` builds the installer, the portable zip and the Claude extension on every push to `main` and publishes them as a release. To build on your own machine you need Rust and, for the installer, WiX 5. See `packaging/windows/package.ps1`.

## Credits and licence

Type and Writer is a fork of [WordCraft](https://github.com/storytold/wordcraft) by the WordCraft contributors. It is licensed under the MIT License or the Apache License 2.0, at your choice, the same as WordCraft. The ArtCraft name and logos belong to the ArtCraft Team and have been removed from this fork. Third-party parts are listed in `ATTRIBUTION.md`:

- PDF reading: hayro (MIT or Apache-2.0)
- OCR: ocrs (MIT or Apache-2.0), with models by Robert Knight (CC-BY-SA-4.0)
- Speech: whisper.cpp (MIT) and OpenAI's Whisper models (MIT)
- Microphone: cpal (Apache-2.0)
