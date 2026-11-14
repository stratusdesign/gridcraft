//! Open and Save through Android's system file picker (any folder, Google Drive included).
//!
//! The picker answers asynchronously, and only to the Java activity
//! (`android/.../GridcraftActivity.java`), which passes the answer to `nativePicked`. GridCraft
//! itself works on files with paths, so each picked document gets a working copy in the app's
//! storage: Open copies the document in and opens the copy; every save GridCraft writes to the
//! copy is copied straight back to the picked document. The copy→document map is kept on disk,
//! so a document from Recent still saves back after a restart.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use gridcraft_ui_egui::{FolderItem, Folders, SheetApp};
use jni::objects::{JClass, JObject, JString, JValue};
use jni::sys::jint;
use jni::{JNIEnv, JavaVM};
use serde::{Deserialize, Serialize};
use serde_json::json;

const OPEN: jint = 1;
const SAVE: jint = 2;
const TREE: jint = 3;
/// A document handed over by "Open with" that GridCraft may only read.
const OPEN_READ_ONLY: jint = 5;

/// Picker answers waiting for the next frame: (kind, document uri, display name).
static PICKED: Mutex<Vec<(jint, String, String)>> = Mutex::new(Vec::new());
/// What a Save As / export is waiting to write: (file name, bytes).
static PENDING_SAVE: Mutex<Option<(String, Vec<u8>)>> = Mutex::new(None);
static CTX: std::sync::OnceLock<egui::Context> = std::sync::OnceLock::new();
/// The Java activity (GridcraftActivity). ndk_context holds the Application instead, which has
/// none of the picker methods.
static ACTIVITY: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

pub fn set_activity(app: &winit::platform::android::activity::AndroidApp) {
    // Set on every start: android_main can run again with a new activity.
    ACTIVITY.store(app.activity_as_ptr() as usize, std::sync::atomic::Ordering::SeqCst);
}

pub fn set_context(ctx: &egui::Context) {
    let _ = CTX.set(ctx.clone());
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_ai_storyteller_gridcraft_GridcraftActivity_nativePicked(
    mut env: JNIEnv,
    _class: JClass,
    kind: jint,
    uri: JString,
    name: JString,
) {
    let read = |env: &mut JNIEnv, s: &JString| -> Option<String> {
        if s.is_null() {
            return None;
        }
        env.get_string(s).ok().map(String::from)
    };
    let uri = read(&mut env, &uri);
    let name = read(&mut env, &name).unwrap_or_else(|| "Book.xlsx".into());
    match uri {
        Some(uri) => {
            if let Ok(mut q) = PICKED.lock() {
                q.push((kind, uri, name));
            }
        }
        // Cancelled: a Save As that was waiting writes nothing.
        None if kind == SAVE => {
            if let Ok(mut p) = PENDING_SAVE.lock() {
                *p = None;
            }
        }
        None => {}
    }
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

/// Calls a method of the activity with string arguments; its string answer (empty for none).
/// `None` when the call failed (logged). Every Java reference made is freed before returning.
pub(crate) fn call(method: &str, sig: &str, strings: &[&str]) -> Option<String> {
    let activity = ACTIVITY.load(std::sync::atomic::Ordering::SeqCst);
    if activity == 0 {
        return None;
    }
    let ctx = ndk_context::android_context();
    // SAFETY: android-activity fills ndk_context with the process's JavaVM.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread_permanently().ok()?;
    let r = env.with_local_frame(16, |env| -> jni::errors::Result<Option<String>> {
        // SAFETY: NativeActivity holds this global reference to the activity for the life of the app.
        let activity = unsafe { JObject::from_raw(activity as jni::sys::jobject) };
        let mut objs = Vec::new();
        for s in strings {
            objs.push(JObject::from(env.new_string(s)?));
        }
        let args: Vec<JValue> = objs.iter().map(JValue::Object).collect();
        let v = match env.call_method(&activity, method, sig, &args) {
            Ok(v) => v,
            Err(e) => {
                if env.exception_check().unwrap_or(false) {
                    let _ = env.exception_describe();
                    let _ = env.exception_clear();
                }
                log::error!("{method}: {e}");
                return Ok(None);
            }
        };
        Ok(Some(match v.l() {
            Ok(obj) if !obj.is_null() => env.get_string(&JString::from(obj)).map(String::from).unwrap_or_default(),
            _ => String::new(),
        }))
    });
    r.ok().flatten()
}

fn sig(args: usize, returns: bool) -> String {
    format!("({}){}", "Ljava/lang/String;".repeat(args), if returns { "Ljava/lang/String;" } else { "V" })
}

/// A call whose answer starts with '+' (the value follows) or '!' (an error follows).
fn ask(method: &str, strings: &[&str]) -> Result<String, String> {
    let a = call(method, &sig(strings.len(), true), strings).ok_or("Android didn't answer")?;
    match a.split_at_checked(1) {
        Some(("+", v)) => Ok(v.to_string()),
        Some(("!", e)) => Err(e.to_string()),
        _ => Err("Android gave no answer".into()),
    }
}

pub fn pick_open() {
    call("pickOpen", "()V", &[]);
}

/// The `download` service: remember the bytes, ask where to save them.
pub fn save_bytes(name: &str, bytes: &[u8]) {
    if let Ok(mut p) = PENDING_SAVE.lock() {
        *p = Some((name.to_string(), bytes.to_vec()));
    }
    call("pickSave", &sig(1, false), &[name]);
}

fn copy(method: &str, a: &str, b: &str) -> Result<(), String> {
    match call(method, &sig(2, true), &[a, b]) {
        Some(e) if e.is_empty() => Ok(()),
        Some(e) => Err(e),
        None => Err("Android didn't answer".into()),
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// A location the user let GridCraft use (a folder tree), or a folder in one used recently.
#[derive(Clone, Serialize, Deserialize)]
struct Place {
    tree: String,
    /// The folder's document id inside `tree` (for a location, its top folder).
    doc: String,
    name: String,
    /// For a recent folder: the location it is in.
    #[serde(default)]
    location: String,
}

impl Place {
    fn item(&self) -> FolderItem {
        FolderItem { id: join(&self.tree, &self.doc), name: self.name.clone(), detail: self.location.clone(), folder: true }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Places {
    places: Vec<Place>,
    recent: Vec<Place>,
}

/// Browser ids carry the location and the document: `tree` + unit separator + `doc`.
fn join(tree: &str, doc: &str) -> String {
    format!("{tree}\u{1f}{doc}")
}

fn split(id: &str) -> Result<(&str, &str), String> {
    id.split_once('\u{1f}').ok_or_else(|| "unknown folder".to_string())
}

/// Working copies and the documents they belong to, and the locations GridCraft may browse.
pub struct Files {
    dir: Option<PathBuf>,
    places: Places,
    /// Save As working copies not yet in their folder: path → (folder, folder name, file name,
    /// file to replace).
    pending: HashMap<String, (String, String, String, Option<String>)>,
    /// working copy path → document uri (persisted in `docs.json`)
    docs: HashMap<String, String>,
    /// working copy path → modification time last copied back
    synced: HashMap<String, Option<SystemTime>>,
    /// Working copies whose copy back failed and is still owed (persisted in `unsynced.json`).
    unsynced: std::collections::HashSet<String>,
    last_check: Instant,
}

impl Files {
    pub fn new(data_dir: Option<PathBuf>) -> Files {
        let dir = data_dir.map(|d| d.join("docs"));
        let docs: HashMap<String, String> =
            dir.as_ref().and_then(|d| std::fs::read(d.join("docs.json")).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let unsynced: std::collections::HashSet<String> =
            dir.as_ref().and_then(|d| std::fs::read(d.join("unsynced.json")).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        // A copy back still owed from last time counts as never written, so it is tried again.
        let synced = docs.keys().map(|p| (p.clone(), if unsynced.contains(p) { None } else { mtime(Path::new(p)) })).collect();
        let places =
            dir.as_ref().and_then(|d| std::fs::read(d.join("places.json")).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        Files { dir, places, pending: HashMap::new(), docs, synced, unsynced, last_check: Instant::now() }
    }

    fn store(&self) {
        if let Some(d) = &self.dir
            && let Ok(b) = serde_json::to_vec_pretty(&self.docs)
        {
            let _ = std::fs::create_dir_all(d);
            let _ = std::fs::write(d.join("docs.json"), b);
        }
        if let Some(d) = &self.dir
            && let Ok(b) = serde_json::to_vec_pretty(&self.places)
        {
            let _ = std::fs::write(d.join("places.json"), b);
        }
        if let Some(d) = &self.dir
            && let Ok(b) = serde_json::to_vec_pretty(&self.unsynced)
        {
            let _ = std::fs::write(d.join("unsynced.json"), b);
        }
    }

    /// Puts a folder at the top of the recent folders.
    fn used(&mut self, tree: &str, doc: &str, name: &str) {
        let location = self.places.places.iter().find(|p| p.tree == tree).map(|p| p.name.clone()).unwrap_or_default();
        self.places.recent.retain(|p| !(p.tree == tree && p.doc == doc));
        self.places.recent.insert(0, Place { tree: tree.into(), doc: doc.into(), name: name.into(), location });
        self.places.recent.truncate(8);
    }

    /// A working copy path for `uri`, named like the document.
    fn copy_path(&self, uri: &str, name: &str) -> Option<PathBuf> {
        if let Some((p, _)) = self.docs.iter().find(|(_, u)| *u == uri) {
            return Some(PathBuf::from(p));
        }
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in uri.bytes() {
            h = (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
        }
        let name: String = name.chars().map(|c| if matches!(c, '/' | '\\' | '\0') { '_' } else { c }).collect();
        let folder = self.dir.as_ref()?.join(format!("{h:016x}"));
        std::fs::create_dir_all(&folder).ok()?;
        Some(folder.join(name))
    }

    /// Each frame: act on picker answers, and copy saved working copies back.
    pub fn logic(&mut self, app: &mut SheetApp) {
        let picked: Vec<_> = PICKED.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
        for (kind, uri, name) in picked {
            if let Err(e) = self.picked(app, kind, &uri, &name) {
                app.message = Some(("GridCraft".into(), format!("{name}: {e}")));
            }
        }
        if self.last_check.elapsed() >= Duration::from_millis(700) {
            self.last_check = Instant::now();
            self.sync_back(app);
        }
    }

    fn picked(&mut self, app: &mut SheetApp, kind: jint, uri: &str, name: &str) -> Result<(), String> {
        if kind == TREE {
            let doc = ask("rootId", &[uri])?;
            self.places.places.retain(|p| p.tree != uri);
            self.places.places.push(Place { tree: uri.into(), doc, name: name.into(), location: String::new() });
            self.store();
            return Ok(());
        }
        let path = self.copy_path(uri, name).ok_or("no room for a working copy")?;
        let p = path.to_string_lossy().into_owned();
        if kind == OPEN || kind == OPEN_READ_ONLY {
            copy("copyIn", uri, &p)?;
            // Read only: saving keeps the working copy; Save As puts it somewhere.
            if kind == OPEN {
                self.docs.insert(p.clone(), uri.to_string());
                self.synced.insert(p.clone(), mtime(&path));
                self.store();
            }
            app.open_path(&p);
            if kind == OPEN_READ_ONLY {
                app.message = Some(("GridCraft".into(), format!("{name} was opened read only. Use Save As to keep your changes.")));
            }
            return Ok(());
        }
        let Some((wanted, bytes)) = PENDING_SAVE.lock().ok().and_then(|mut s| s.take()) else { return Ok(()) };
        let workbook = Path::new(&wanted).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("xlsx"));
        if workbook {
            // Save As: the workbook now belongs to the picked document; GridCraft saves to its
            // working copy and the next check copies that back.
            self.docs.insert(p.clone(), uri.to_string());
            self.synced.insert(p.clone(), None);
            self.store();
            app.run_or_alert("file.save", json!({"path": p}));
            self.sync_back(app);
        } else {
            // An export (CSV, web page, PDF) is a copy: write it and keep nothing.
            std::fs::write(&path, &bytes).map_err(|e| e.to_string())?;
            copy("copyOut", &p, uri)?;
        }
        Ok(())
    }

    /// Copies every working copy saved since the last check back to its document.
    pub fn sync_back(&mut self, app: &mut SheetApp) {
        let mut failed = None;
        for (p, uri) in &self.docs {
            let now = mtime(Path::new(p));
            if now.is_none() || self.synced.get(p).copied().flatten() == now {
                continue;
            }
            match copy("copyOut", p, uri) {
                Ok(()) => {
                    self.synced.insert(p.clone(), now);
                    if self.unsynced.remove(p) {
                        self.store();
                    }
                }
                // Not marked written: tried again at every check, and after a restart.
                Err(e) => {
                    if self.unsynced.insert(p.clone()) {
                        self.store();
                        failed = Some(format!("Saved here, but it couldn't be written back to the original file yet (trying again): {e}"));
                    }
                }
            }
        }
        if let Some(m) = failed {
            app.message = Some(("GridCraft".into(), m));
        }
    }
}

fn doc_uri(tree: &str, doc: &str) -> Result<String, String> {
    call("docUri", &sig(2, true), &[tree, doc]).filter(|u| !u.is_empty()).ok_or_else(|| "Android didn't answer".to_string())
}

/// The Open and Save As pages' folder browser, over the locations in [`Files`].
pub struct AndroidFolders(pub Rc<RefCell<Files>>);

impl Folders for AndroidFolders {
    fn places(&self) -> Vec<FolderItem> {
        self.0.borrow().places.places.iter().map(Place::item).collect()
    }

    fn recent(&self) -> Vec<FolderItem> {
        self.0.borrow().places.recent.iter().map(Place::item).collect()
    }

    fn add_place(&self) {
        call("pickTree", "()V", &[]);
    }

    fn list(&self, folder: &str) -> Result<Vec<FolderItem>, String> {
        let (tree, doc) = split(folder)?;
        let list: Vec<(String, String, bool)> = serde_json::from_str(&ask("list", &[tree, doc])?).map_err(|e| e.to_string())?;
        Ok(list.into_iter().map(|(id, name, folder)| FolderItem { id: join(tree, &id), name, detail: String::new(), folder }).collect())
    }

    fn open(&self, folder: &str, folder_name: &str, file: &FolderItem) -> Result<String, String> {
        let (tree, doc) = split(&file.id)?;
        let uri = doc_uri(tree, doc)?;
        let mut files = self.0.borrow_mut();
        let path = files.copy_path(&uri, &file.name).ok_or("no room for a working copy")?;
        let p = path.to_string_lossy().into_owned();
        copy("copyIn", &uri, &p)?;
        files.docs.insert(p.clone(), uri);
        files.synced.insert(p.clone(), mtime(&path));
        if let Ok((t, d)) = split(folder) {
            files.used(t, d, folder_name);
        }
        files.store();
        Ok(p)
    }

    fn save(&self, folder: &str, folder_name: &str, name: &str, existing: Option<&FolderItem>) -> Result<String, String> {
        split(folder)?;
        let mut files = self.0.borrow_mut();
        // A fresh working copy; the document is made (or replaced) only once this is written.
        let path = files.copy_path(&format!("{folder}\u{1f}{name}\u{1f}{:?}", SystemTime::now()), name).ok_or("no room for a working copy")?;
        let p = path.to_string_lossy().into_owned();
        files.pending.insert(p.clone(), (folder.to_string(), folder_name.to_string(), name.to_string(), existing.map(|f| f.id.clone())));
        Ok(p)
    }

    fn saved(&self, path: &str) -> Result<(), String> {
        let mut files = self.0.borrow_mut();
        let (folder, folder_name, name, existing) = files.pending.remove(path).ok_or("nothing to save")?;
        let (tree, doc) = split(&folder)?;
        let uri = match &existing {
            Some(id) => {
                let (t, d) = split(id)?;
                doc_uri(t, d)?
            }
            None => ask("create", &[tree, doc, &name])?,
        };
        copy("copyOut", path, &uri)?;
        files.docs.insert(path.to_string(), uri);
        files.synced.insert(path.to_string(), mtime(Path::new(path)));
        files.used(tree, doc, &folder_name);
        files.store();
        Ok(())
    }
}
