use super::printing;

type PickSave = Box<dyn Fn(&str) -> Option<String>>;
type OpenUrl = Box<dyn Fn(&str)>;
type CopyHtml = Box<dyn FnMut(&str, &str) -> Result<(), String>>;
type Download = Box<dyn Fn(&str, &[u8])>;
type PrintPdf = Box<dyn Fn(&[u8])>;

/// Host services the platform shell provides (file pickers, opening URLs).
#[derive(Default)]
pub struct Services {
    /// Desktop: discover configured printers on a worker.
    pub printers: Option<printing::Discover>,
    /// Desktop: submit the PDF with the chosen settings on a worker.
    pub print_native: Option<printing::Submit>,
    pub pick_open: Option<Box<dyn Fn() -> Option<String>>>,
    pub pick_save: Option<PickSave>,
    /// Native picture picker (PNG/JPEG), separate from workbook opening.
    pub pick_picture: Option<Box<dyn Fn() -> Option<String>>>,
    /// Browser picture picker. Each dialog owns its inbox, so canceled picks cannot
    /// insert into a subsequent dialog or open the image as a workbook.
    pub pick_picture_async: Option<Box<dyn Fn(Inbox)>>,
    pub open_url: Option<OpenUrl>,
    /// Publish HTML and its plain-text alternative together. `Ok` means the host owns the
    /// write (including any asynchronous fallback); an immediate error uses egui's text path.
    pub copy_html: Option<CopyHtml>,
    /// Read the HTML flavour of the system clipboard when the user pastes text, so cells the
    /// source spreadsheet recorded as numbers stay numbers ("Rp10,000.00" → 10000). `None` from
    /// the host pastes the plain text alone.
    pub paste_html: Option<Box<dyn FnMut() -> Option<String>>>,
    /// Read the system clipboard's plain text for the ribbon and menu Paste commands (Cmd/Ctrl+V
    /// gets it from egui's `Event::Paste`). `None` from the host, or no service (web), pastes
    /// GridCraft's own clipboard.
    pub paste_text: Option<Box<dyn FnMut() -> Option<String>>>,
    /// Read the picture on the system clipboard for the ribbon and menu Paste commands when it
    /// holds no text (Cmd/Ctrl+V gets it from egui's `Event::PasteImage`).
    pub paste_image: Option<Box<dyn FnMut() -> Option<image::RgbaImage>>>,
    /// Web: deliver a file to the user (name, bytes).
    pub download: Option<Download>,
    /// Web: start an asynchronous file pick; the bytes arrive later in `inbox`.
    pub open_async: Option<Box<dyn Fn()>>,
    /// Files delivered asynchronously (name, bytes), opened on the next frame.
    pub inbox: Option<Inbox>,
    /// Web: hand PDF bytes to the browser's print flow directly (no download,
    /// no intermediate file) — opens the system print dialog on that PDF.
    pub print: Option<PrintPdf>,
    /// Android: GridCraft's own folder browser on the Open and Save As pages.
    pub folders: Option<Box<dyn Folders>>,
}

/// A location, folder or file in a [`Folders`] browser. `id` means something only to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FolderItem {
    pub id: String,
    pub name: String,
    /// Where it is, for locations and recent folders ("My files › Documents").
    pub detail: String,
    pub folder: bool,
}

/// Android: the folders the user let GridCraft use, browsed on the Open and Save As pages
/// instead of a system picker (ChromeOS shows its own picker only for Open).
pub trait Folders {
    /// The locations the user let GridCraft use.
    fn places(&self) -> Vec<FolderItem>;
    /// Folders opened from or saved to, newest first.
    fn recent(&self) -> Vec<FolderItem>;
    /// Asks the user for another location; it appears in `places` once chosen.
    fn add_place(&self);
    /// A folder's subfolders and files.
    fn list(&self, folder: &str) -> Result<Vec<FolderItem>, String>;
    /// A local path to open `file` (in `folder`, called `folder_name`) from; saving it saves the
    /// file.
    fn open(&self, folder: &str, folder_name: &str, file: &FolderItem) -> Result<String, String>;
    /// A local path to save `name` into `folder` at; what is written there goes to the folder.
    /// `existing` is the file of that name to replace.
    fn save(&self, folder: &str, folder_name: &str, name: &str, existing: Option<&FolderItem>) -> Result<String, String>;
}

/// Shared queue of files read asynchronously (browser file picker, drag and drop).
pub type Inbox = std::sync::Arc<std::sync::Mutex<Vec<(String, Vec<u8>)>>>;

/// Which parts of the app's own window chrome are drawn. Everything is on by default; a host
/// that embeds the sheet in its own window (and owns files itself) can turn off the title bar
/// and the File tab, whose Backstage view then never opens. Not persisted: it describes the
/// host, not a user preference.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    /// The title bar: AutoSave, Home, Save, Undo/Redo, the file name, Comments, Share, Search and
    /// the theme switch.
    pub title_bar: bool,
    /// The ribbon's File tab and the Backstage view it opens (also reached by keyboard and
    /// commands such as `ui.backstage`).
    pub file_tab: bool,
}

impl Default for Chrome {
    fn default() -> Self {
        Chrome { title_bar: true, file_tab: true }
    }
}
