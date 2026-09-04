//! An in-memory Typst [`World`].
//!
//! Nothing here touches the filesystem. The world serves exactly four kinds of
//! files, all from memory:
//!
//! * `/main.typ` — the document template (`include_str!`, parsed once into a
//!   [`Source`] and kept).
//! * `/data.json` — the [`crate::model::DocumentModel`], swapped on every
//!   compile via [`TypstWorld::set_data`]. This is the *only* input that
//!   changes between compiles, so `comemo` re-validates just the `json()` call
//!   and the layout — fonts, the library and the parsed template all stay
//!   cached.
//! * `/assets/turnen.*` — the swoosh, embedded once. The org emblem at
//!   `/assets/org.*` is swapped per compile via [`TypstWorld::set_emblem`]
//!   (it depends on the competition's `OrganizationDTO`).
//! * fonts — the Archivo faces embedded in `dtb-ke-resource`.

use std::sync::Mutex;

use chrono::Datelike;
use typst::diag::{FileError, FileResult};
use typst::foundations::{Bytes, Datetime, Duration};
use typst::syntax::{FileId, RootedPath, Source, VirtualPath, VirtualRoot};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::{Library, LibraryExt, World};

use crate::ExportError;

/// The virtual path of the template's entry file.
const MAIN: &str = "/main.typ";
/// The virtual path the template reads the model from.
const DATA: &str = "/data.json";

const TEMPLATE: &str = include_str!("template/document.typ");

pub(crate) struct TypstWorld {
    library: LazyHash<Library>,
    book: LazyHash<FontBook>,
    fonts: Vec<Font>,

    main: FileId,
    template: Source,

    data_id: FileId,
    data: Mutex<Bytes>,

    /// `/assets/turnen.*` — immutable for the life of the world.
    assets: Vec<Asset>,

    /// `/assets/org.{svg,png,jpg}` — one `FileId` per candidate extension.
    emblem_ids: [(FileId, &'static str); 3],
    /// `(index into `emblem_ids`, bytes)` for the current competition, if any.
    emblem: Mutex<Option<(usize, Bytes)>>,

    today: Option<Datetime>,
}

struct Asset {
    name: &'static str,
    id: FileId,
    vpath: String,
    bytes: Bytes,
}

impl TypstWorld {
    pub(crate) fn new() -> Result<Self, ExportError> {
        let mut fonts = Vec::new();
        for blob in dtb_ke_resource::fonts() {
            let bytes = Bytes::new(blob.into_owned());
            fonts.extend(Font::iter(bytes));
        }
        if fonts.is_empty() {
            log::error!("Typst world: no embedded fonts — export is unavailable");
            return Err(ExportError::NoFonts);
        }
        log::trace!("Typst world: {} font face(s) loaded", fonts.len());

        let book = FontBook::from_fonts(fonts.iter());

        let main = file_id(MAIN)?;
        let data_id = file_id(DATA)?;

        let assets = collect_assets()?;
        let emblem_ids = [
            (file_id("/assets/org.svg")?, "svg"),
            (file_id("/assets/org.png")?, "png"),
            (file_id("/assets/org.jpg")?, "jpg"),
        ];

        let now = chrono::Local::now().date_naive();
        let today = Datetime::from_ymd(now.year(), now.month() as u8, now.day() as u8);

        Ok(Self {
            library: LazyHash::new(Library::builder().build()),
            book: LazyHash::new(book),
            fonts,
            main,
            template: Source::new(main, TEMPLATE.to_owned()),
            data_id,
            data: Mutex::new(Bytes::new(b"{}".to_vec())),
            assets,
            emblem_ids,
            emblem: Mutex::new(None),
            today,
        })
    }

    /// Point `/data.json` at a fresh model. Cheap: only swaps an `Arc`-backed
    /// [`Bytes`]; `comemo` notices the changed hash on the next compile.
    pub(crate) fn set_data(&self, json: &str) {
        *self.data.lock().unwrap() = Bytes::new(json.as_bytes().to_vec());
    }

    /// The `/assets/…` virtual path of the named immutable asset (`"turnen"`),
    /// or `None` if it was not embedded.
    pub(crate) fn asset_path(&self, name: &str) -> Option<String> {
        self.assets
            .iter()
            .find(|a| a.name == name)
            .map(|a| a.vpath.clone())
    }

    /// Point `/assets/org.*` at this competition's emblem (`None` clears it, so
    /// the template falls back to the DTB word-mark). The extension is sniffed
    /// from the bytes so Typst's image loader picks the right decoder.
    pub(crate) fn set_emblem(&self, bytes: Option<Vec<u8>>) {
        *self.emblem.lock().unwrap() = bytes.map(|b| {
            let idx = if b.starts_with(b"\x89PNG") {
                1
            } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
                2
            } else {
                0 // svg / anything else
            };
            (idx, Bytes::new(b))
        });
    }

    /// The `/assets/org.{ext}` path for the currently-set emblem, if any.
    pub(crate) fn emblem_path(&self) -> Option<String> {
        let guard = self.emblem.lock().unwrap();
        guard
            .as_ref()
            .map(|(idx, _)| format!("/assets/org.{}", self.emblem_ids[*idx].1))
    }
}

impl World for TypstWorld {
    fn library(&self) -> &LazyHash<Library> {
        &self.library
    }

    fn book(&self) -> &LazyHash<FontBook> {
        &self.book
    }

    fn main(&self) -> FileId {
        self.main
    }

    fn source(&self, id: FileId) -> FileResult<Source> {
        if id == self.main {
            Ok(self.template.clone())
        } else {
            Err(FileError::NotFound(vpath_display(id)))
        }
    }

    fn file(&self, id: FileId) -> FileResult<Bytes> {
        if id == self.data_id {
            return Ok(self.data.lock().unwrap().clone());
        }
        if let Some(asset) = self.assets.iter().find(|a| a.id == id) {
            return Ok(asset.bytes.clone());
        }
        if let Some((idx, bytes)) = self.emblem.lock().unwrap().as_ref()
            && self.emblem_ids[*idx].0 == id
        {
            return Ok(bytes.clone());
        }
        Err(FileError::NotFound(vpath_display(id)))
    }

    fn font(&self, index: usize) -> Option<Font> {
        self.fonts.get(index).cloned()
    }

    fn today(&self, _offset: Option<Duration>) -> Option<Datetime> {
        self.today
    }
}

fn file_id(vpath: &str) -> Result<FileId, ExportError> {
    let vpath = VirtualPath::new(vpath)
        .map_err(|e| ExportError::Template(format!("bad virtual path {vpath:?}: {e}")))?;
    Ok(FileId::new(RootedPath::new(VirtualRoot::Project, vpath)))
}

fn vpath_display(id: FileId) -> std::path::PathBuf {
    std::path::PathBuf::from(id.vpath().get_without_slash())
}

fn collect_assets() -> Result<Vec<Asset>, ExportError> {
    let mut out = Vec::new();

    // Swoosh, top-right — always the same. (The top-left org emblem is swapped
    // per compile: see `TypstWorld::set_emblem`.)
    if let Some(bytes) = dtb_ke_resource::logo_turnen() {
        out.push(asset("turnen", &bytes)?);
    }

    Ok(out)
}

/// Register `bytes` at `/assets/{name}.{ext}`, picking the extension from the
/// content so Typst's image loader detects the format correctly.
fn asset(name: &'static str, bytes: &[u8]) -> Result<Asset, ExportError> {
    let ext = if bytes.starts_with(b"\x89PNG") {
        "png"
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else {
        "svg"
    };
    let vpath = format!("/assets/{name}.{ext}");
    let id = file_id(&vpath)?;
    Ok(Asset {
        name,
        id,
        vpath,
        bytes: Bytes::new(bytes.to_vec()),
    })
}
