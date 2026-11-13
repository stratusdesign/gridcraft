package ai.storyteller.gridcraft;

import android.app.NativeActivity;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.provider.DocumentsContract;
import android.provider.OpenableColumns;

import java.io.FileInputStream;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;

/**
 * GridCraft's window (a NativeActivity running the Rust app) plus the one thing Rust can't do
 * alone on Android: the system file picker, whose answer only ever reaches a Java activity.
 * Rust calls pickOpen/pickSave, the answer comes back through nativePicked, and Rust copies the
 * document in and out of its working copy with copyIn/copyOut.
 */
public class GridcraftActivity extends NativeActivity {
    static {
        // Loaded here as well as by NativeActivity, so the JVM can find nativePicked.
        System.loadLibrary("gridcraft_android");
    }

    static final int OPEN = 1;
    static final int SAVE = 2;
    static final int TREE = 3;
    static final int RW = Intent.FLAG_GRANT_READ_URI_PERMISSION | Intent.FLAG_GRANT_WRITE_URI_PERMISSION;

    static native void nativePicked(int kind, String uri, String name);

    static final String[] TYPES = {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "application/vnd.ms-excel.sheet.macroEnabled.12",
        "application/vnd.ms-excel",
        "application/vnd.ms-excel.sheet.binary.macroEnabled.12",
        "application/vnd.oasis.opendocument.spreadsheet",
        "text/csv", "text/comma-separated-values", "text/tab-separated-values", "text/plain",
        "application/json", "application/octet-stream",
    };

    static String mimeFor(String name) {
        String n = name.toLowerCase();
        if (n.endsWith(".csv")) return "text/csv";
        if (n.endsWith(".html")) return "text/html";
        if (n.endsWith(".pdf")) return "application/pdf";
        return TYPES[0];
    }

    public void pickOpen() {
        runOnUiThread(() -> {
            Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            i.addCategory(Intent.CATEGORY_OPENABLE);
            i.setType("*/*");
            i.putExtra(Intent.EXTRA_MIME_TYPES, TYPES);
            i.addFlags(RW | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            startInLastFolder(i);
            startActivityForResult(i, OPEN);
        });
    }

    public void pickSave(String name) {
        runOnUiThread(() -> {
            Intent i = new Intent(Intent.ACTION_CREATE_DOCUMENT);
            i.addCategory(Intent.CATEGORY_OPENABLE);
            i.setType(mimeFor(name));
            i.putExtra(Intent.EXTRA_TITLE, name);
            i.addFlags(RW | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            startInLastFolder(i);
            startActivityForResult(i, SAVE);
        });
    }

    /** Add a location: the user picks a folder GridCraft may use (My files, Drive, …). */
    public void pickTree() {
        runOnUiThread(() -> {
            Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT_TREE);
            i.addFlags(RW | Intent.FLAG_GRANT_PERSISTABLE_URI_PERMISSION);
            startActivityForResult(i, TREE);
        });
    }

    // Folder browsing for GridCraft's own Open and Save As pages. Answers start with '+' (the
    // value follows) or '!' (an error message follows).

    /** The top folder of a location. */
    public String rootId(String tree) {
        try {
            return "+" + DocumentsContract.getTreeDocumentId(Uri.parse(tree));
        } catch (Exception e) {
            return "!" + e.getMessage();
        }
    }

    /** A folder's contents: one line per item: id, name, d (folder) or f (file), split by unit separators. */
    public String list(String tree, String folder) {
        Uri t = Uri.parse(tree);
        Uri children = DocumentsContract.buildChildDocumentsUriUsingTree(t, folder);
        String[] cols = {
            DocumentsContract.Document.COLUMN_DOCUMENT_ID,
            DocumentsContract.Document.COLUMN_DISPLAY_NAME,
            DocumentsContract.Document.COLUMN_MIME_TYPE,
        };
        StringBuilder out = new StringBuilder("+");
        try (Cursor c = getContentResolver().query(children, cols, null, null, null)) {
            if (c == null) return "!this folder can't be read";
            while (c.moveToNext()) {
                boolean dir = DocumentsContract.Document.MIME_TYPE_DIR.equals(c.getString(2));
                out.append(c.getString(0)).append('\u001f').append(c.getString(1)).append('\u001f').append(dir ? 'd' : 'f').append('\n');
            }
            return out.toString();
        } catch (Exception e) {
            return "!" + e.getMessage();
        }
    }

    /** The document uri of `doc` inside a location. */
    public String docUri(String tree, String doc) {
        return DocumentsContract.buildDocumentUriUsingTree(Uri.parse(tree), doc).toString();
    }

    /** A new file `name` in `folder`; its document uri. */
    public String create(String tree, String folder, String name) {
        try {
            Uri parent = DocumentsContract.buildDocumentUriUsingTree(Uri.parse(tree), folder);
            Uri made = DocumentsContract.createDocument(getContentResolver(), parent, mimeFor(name), name);
            return made == null ? "!the file couldn't be made" : "+" + made;
        } catch (Exception e) {
            return "!" + e.getMessage();
        }
    }

    @Override
    protected void onActivityResult(int request, int result, Intent data) {
        super.onActivityResult(request, result, data);
        Uri uri = (result == RESULT_OK && data != null) ? data.getData() : null;
        String name = null;
        if (uri != null) {
            try {
                // Keeps access after a restart, so Save still reaches the file.
                getContentResolver().takePersistableUriPermission(uri, data.getFlags() & RW);
            } catch (Exception ignored) {
            }
            name = request == TREE ? treeName(uri) : displayName(uri);
            if (request != TREE) getPreferences(MODE_PRIVATE).edit().putString("lastPicked", uri.toString()).apply();
        }
        nativePicked(request, uri == null ? null : uri.toString(), name);
    }

    /** The picker opens where the last picked file was (Documents, Drive, …), not in Downloads. */
    void startInLastFolder(Intent i) {
        String last = getPreferences(MODE_PRIVATE).getString("lastPicked", null);
        if (last != null) i.putExtra(DocumentsContract.EXTRA_INITIAL_URI, Uri.parse(last));
    }

    String treeName(Uri tree) {
        try {
            return displayName(DocumentsContract.buildDocumentUriUsingTree(tree, DocumentsContract.getTreeDocumentId(tree)));
        } catch (Exception e) {
            return "Folder";
        }
    }

    String displayName(Uri uri) {
        try (Cursor c = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null)) {
            if (c != null && c.moveToFirst() && !c.isNull(0)) return c.getString(0);
        } catch (Exception ignored) {
        }
        String last = uri.getLastPathSegment();
        return last == null ? "Book.xlsx" : last.substring(last.lastIndexOf('/') + 1);
    }

    /** The picked document into the working copy at `path`. Empty string = success. */
    public String copyIn(String uri, String path) {
        try (InputStream in = getContentResolver().openInputStream(Uri.parse(uri));
             OutputStream out = new FileOutputStream(path)) {
            if (in == null) return "the file couldn't be read";
            pump(in, out);
            return "";
        } catch (Exception e) {
            return String.valueOf(e.getMessage());
        }
    }

    /** The working copy at `path` back into the picked document. Empty string = success. */
    public String copyOut(String path, String uri) {
        Uri u = Uri.parse(uri);
        OutputStream out = null;
        try {
            try {
                out = getContentResolver().openOutputStream(u, "wt");
            } catch (Exception e) {
                out = getContentResolver().openOutputStream(u, "w");
            }
            if (out == null) return "the file couldn't be written";
            try (InputStream in = new FileInputStream(path)) {
                pump(in, out);
            }
            out.close();
            return "";
        } catch (Exception e) {
            return String.valueOf(e.getMessage());
        }
    }

    static void pump(InputStream in, OutputStream out) throws java.io.IOException {
        byte[] buf = new byte[65536];
        for (int n; (n = in.read(buf)) > 0; ) out.write(buf, 0, n);
    }
}
