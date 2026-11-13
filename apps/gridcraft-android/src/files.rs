//! Open and Save through Android's system file picker (any folder, Google Drive included).
//!
//! The picker answers asynchronously, and only to the Java activity
//! (`android/.../GridcraftActivity.java`), which passes the answer to `nativePicked`. GridCraft
//! itself works on files with paths, so each picked document gets a working copy in the app's
//! storage: Open copies the document in and opens the copy; every save GridCraft writes to the
//! copy is copied straight back to the picked document. The copy→document map is kept on disk,
//! so a document from Recent still saves back after a restart.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

use gridcraft_ui_egui::SheetApp;
use jni::objects::{JClass, JObject, JString, JValue};
use jni::sys::jint;
use jni::{JNIEnv, JavaVM};
use serde_json::json;

const OPEN: jint = 1;
const SAVE: jint = 2;

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

/// Calls a method on the activity; `None` when JNI failed (logged).
fn call_activity(method: &str, sig: &str, args: &[JValue]) -> Option<String> {
    let ctx = ndk_context::android_context();
    // SAFETY: android-activity fills ndk_context with the process's JavaVM and the activity.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread_permanently().ok()?;
    let activity = ACTIVITY.load(std::sync::atomic::Ordering::SeqCst);
    if activity == 0 {
        return None;
    }
    // SAFETY: NativeActivity holds this global reference to the activity for the life of the app.
    let activity = unsafe { JObject::from_raw(activity as jni::sys::jobject) };
    let r = env.call_method(&activity, method, sig, args);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    match r {
        Ok(v) => match v.l() {
            Ok(obj) if !obj.is_null() => env.get_string(&JString::from(obj)).ok().map(String::from),
            _ => Some(String::new()),
        },
        Err(e) => {
            log::error!("{method}: {e}");
            None
        }
    }
}

fn with_strings<R>(strings: &[&str], f: impl FnOnce(&[JValue]) -> R) -> Option<R> {
    let ctx = ndk_context::android_context();
    // SAFETY: as in `call_activity`.
    let vm = unsafe { JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let env = vm.attach_current_thread_permanently().ok()?;
    let objs: Vec<JObject> = strings.iter().filter_map(|s| env.new_string(s).ok().map(JObject::from)).collect();
    if objs.len() != strings.len() {
        return None;
    }
    let args: Vec<JValue> = objs.iter().map(JValue::Object).collect();
    Some(f(&args))
}

pub fn pick_open() {
    call_activity("pickOpen", "()V", &[]);
}

/// The `download` service: remember the bytes, ask where to save them.
pub fn save_bytes(name: &str, bytes: &[u8]) {
    if let Ok(mut p) = PENDING_SAVE.lock() {
        *p = Some((name.to_string(), bytes.to_vec()));
    }
    with_strings(&[name], |args| call_activity("pickSave", "(Ljava/lang/String;)V", args));
}

fn copy(method: &str, a: &str, b: &str) -> Result<(), String> {
    match with_strings(&[a, b], |args| call_activity(method, "(Ljava/lang/String;Ljava/lang/String;)Ljava/lang/String;", args)).flatten() {
        Some(e) if e.is_empty() => Ok(()),
        Some(e) => Err(e),
        None => Err("Android didn't answer".into()),
    }
}

fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// Working copies and the documents they belong to.
pub struct Files {
    dir: Option<PathBuf>,
    /// working copy path → document uri (persisted in `docs.json`)
    docs: HashMap<String, String>,
    /// working copy path → modification time last copied back
    synced: HashMap<String, Option<SystemTime>>,
    last_check: Instant,
}

impl Files {
    pub fn new(data_dir: Option<PathBuf>) -> Files {
        let dir = data_dir.map(|d| d.join("docs"));
        let docs: HashMap<String, String> =
            dir.as_ref().and_then(|d| std::fs::read(d.join("docs.json")).ok()).and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        let synced = docs.keys().map(|p| (p.clone(), mtime(Path::new(p)))).collect();
        Files { dir, docs, synced, last_check: Instant::now() }
    }

    fn store(&self) {
        if let Some(d) = &self.dir
            && let Ok(b) = serde_json::to_vec_pretty(&self.docs)
        {
            let _ = std::fs::create_dir_all(d);
            let _ = std::fs::write(d.join("docs.json"), b);
        }
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
        let path = self.copy_path(uri, name).ok_or("no room for a working copy")?;
        let p = path.to_string_lossy().into_owned();
        if kind == OPEN {
            copy("copyIn", uri, &p)?;
            self.docs.insert(p.clone(), uri.to_string());
            self.synced.insert(p.clone(), mtime(&path));
            self.store();
            app.open_path(&p);
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

    fn sync_back(&mut self, app: &mut SheetApp) {
        let mut failed = None;
        for (p, uri) in &self.docs {
            let now = mtime(Path::new(p));
            if now.is_none() || self.synced.get(p).copied().flatten() == now {
                continue;
            }
            match copy("copyOut", p, uri) {
                Ok(()) => {
                    self.synced.insert(p.clone(), now);
                }
                Err(e) => {
                    self.synced.insert(p.clone(), now);
                    failed = Some(format!("Saved here, but it couldn't be written back to the original file: {e}"));
                }
            }
        }
        if let Some(m) = failed {
            app.message = Some(("GridCraft".into(), m));
        }
    }
}
