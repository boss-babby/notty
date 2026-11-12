package com.lumen.app;

import android.Manifest;
import android.app.NativeActivity;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.content.res.Configuration;
import android.database.Cursor;
import android.graphics.Color;
import android.graphics.Insets;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.VibrationEffect;
import android.os.Vibrator;
import android.print.PrintAttributes;
import android.print.PrintManager;
import android.provider.OpenableColumns;
import android.system.ErrnoException;
import android.system.Os;
import android.view.DisplayCutout;
import android.view.KeyEvent;
import android.view.View;
import android.view.Window;
import android.view.WindowInsets;
import android.view.WindowInsetsController;
import android.view.WindowManager;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.webkit.WebView;
import android.webkit.WebViewClient;
import android.widget.FrameLayout;
import android.widget.Toast;

import java.io.ByteArrayOutputStream;
import java.io.File;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.TimeZone;

/**
 * Lumen's Android shell. The UI itself is Rust (egui, see src/android.rs); this activity provides everything the Android framework
 * has to do:
 *
 *  - edge-to-edge window + exact window insets (status bar, cutout, navigation bar, keyboard) reported to Rust,
 *  - the soft keyboard: a 1x1 View with a real InputConnection (text, composing text, backspace, enter),
 *  - Storage Access Framework file dialogs (open / save), clipboard, links, haptics, notifications, printing, Back.
 *
 * It extends NativeActivity, so the Rust library (liblumen.so) is started by the framework exactly like before.
 */
public class LumenActivity extends NativeActivity {
    static {
        // NativeActivity dlopen()s the library itself, but JNI `native` methods of *this* class are only resolved for libraries that were
        // loaded through the class loader.
        System.loadLibrary("lumen");
    }

    private static final int REQ_OPEN = 7001;
    private static final int REQ_SAVE = 7002;
    private static final String CHANNEL = "lumen-reminders";

    // Rust callbacks (src/android.rs)
    private static native void nativeMetrics(float density, int tzMinutes);
    private static native void nativeInsets(int left, int top, int right, int bottom, boolean imeVisible);
    private static native void nativeText(String text);
    private static native void nativeKey(int androidKeyCode, boolean down);
    private static native void nativeBack();
    private static native void nativePicked(int req, String name, String mime, byte[] data);
    private static native void nativePickCancelled(int req);

    private ImeView ime;
    private InputMethodManager imm;
    private int pendingPick = -1;
    private String pendingSaveName;
    private String pendingSaveMime;
    private byte[] pendingSaveData;
    private WebView printView;

    // ───────────────────────────── lifecycle ─────────────────────────────

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        // Environment for the Rust side (std::env): where to keep data, temp files, the time zone.
        try {
            String files = getFilesDir().getAbsolutePath();
            Os.setenv("HOME", files, true);
            Os.setenv("XDG_DATA_HOME", files, true);
            Os.setenv("LUMEN_DATA_DIR", files, true);
            Os.setenv("TMPDIR", getCacheDir().getAbsolutePath(), true);
            Os.setenv("LUMEN_TZ_MIN", String.valueOf(tzMinutes()), true);
        } catch (ErrnoException ignored) {
        }

        configureWindow();
        super.onCreate(savedInstanceState);

        imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
        ime = new ImeView(this);
        addContentView(ime, new FrameLayout.LayoutParams(1, 1));

        final View decor = getWindow().getDecorView();
        if (Build.VERSION.SDK_INT < 30) {
            decor.setSystemUiVisibility(View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                    | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION);
        }
        decor.setOnApplyWindowInsetsListener(new View.OnApplyWindowInsetsListener() {
            @Override
            public WindowInsets onApplyWindowInsets(View v, WindowInsets insets) {
                dispatchInsets(insets);
                return insets;
            }
        });
        decor.requestApplyInsets();

        if (Build.VERSION.SDK_INT >= 33) {
            getOnBackInvokedDispatcher().registerOnBackInvokedCallback(
                    android.window.OnBackInvokedDispatcher.PRIORITY_DEFAULT,
                    new android.window.OnBackInvokedCallback() {
                        @Override
                        public void onBackInvoked() {
                            nativeBack();
                        }
                    });
        }
        reportMetrics();
    }

    /** Edge-to-edge: the surface covers the whole screen, Rust keeps its content inside the insets we report. */
    @SuppressWarnings("deprecation")
    private void configureWindow() {
        Window w = getWindow();
        w.addFlags(WindowManager.LayoutParams.FLAG_DRAWS_SYSTEM_BAR_BACKGROUNDS);
        w.setStatusBarColor(Color.TRANSPARENT);
        w.setNavigationBarColor(Color.TRANSPARENT);
        if (Build.VERSION.SDK_INT >= 29) {
            w.setStatusBarContrastEnforced(false);
            w.setNavigationBarContrastEnforced(false);
        }
        if (Build.VERSION.SDK_INT >= 28) {
            WindowManager.LayoutParams lp = w.getAttributes();
            lp.layoutInDisplayCutoutMode = WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES;
            w.setAttributes(lp);
        }
        if (Build.VERSION.SDK_INT >= 30) {
            w.setDecorFitsSystemWindows(false);
        }
        // adjustResize makes the system report the keyboard as an inset (we do the layout ourselves in Rust)
        w.setSoftInputMode(WindowManager.LayoutParams.SOFT_INPUT_ADJUST_RESIZE
                | WindowManager.LayoutParams.SOFT_INPUT_STATE_ALWAYS_HIDDEN);
    }

    @SuppressWarnings("deprecation")
    private void dispatchInsets(WindowInsets wi) {
        int l, t, r, b;
        boolean imeVisible;
        if (Build.VERSION.SDK_INT >= 30) {
            Insets bars = wi.getInsets(WindowInsets.Type.systemBars() | WindowInsets.Type.displayCutout());
            Insets im = wi.getInsets(WindowInsets.Type.ime());
            l = bars.left;
            t = bars.top;
            r = bars.right;
            b = Math.max(bars.bottom, im.bottom);
            imeVisible = wi.isVisible(WindowInsets.Type.ime());
        } else {
            l = wi.getSystemWindowInsetLeft();
            t = wi.getSystemWindowInsetTop();
            r = wi.getSystemWindowInsetRight();
            b = wi.getSystemWindowInsetBottom(); // includes the keyboard with adjustResize
            if (Build.VERSION.SDK_INT >= 28) {
                DisplayCutout dc = wi.getDisplayCutout();
                if (dc != null) {
                    l = Math.max(l, dc.getSafeInsetLeft());
                    t = Math.max(t, dc.getSafeInsetTop());
                    r = Math.max(r, dc.getSafeInsetRight());
                    b = Math.max(b, dc.getSafeInsetBottom());
                }
            }
            int nav = wi.getStableInsetBottom();
            imeVisible = (b - nav) > (int) (100 * getResources().getDisplayMetrics().density);
        }
        nativeInsets(l, t, r, b, imeVisible);
    }

    private int tzMinutes() {
        return TimeZone.getDefault().getOffset(System.currentTimeMillis()) / 60000;
    }

    private void reportMetrics() {
        nativeMetrics(getResources().getDisplayMetrics().density, tzMinutes());
    }

    @Override
    public void onConfigurationChanged(Configuration newConfig) {
        super.onConfigurationChanged(newConfig);
        reportMetrics();
        getWindow().getDecorView().requestApplyInsets();
    }

    @Override
    protected void onResume() {
        super.onResume();
        reportMetrics();
        getWindow().getDecorView().requestApplyInsets();
    }

    @Override
    protected void onPause() {
        hideImeNow();
        super.onPause();
    }

    @Override
    public void onBackPressed() {
        // never finish(): Rust decides (close drawer / dialog, go up a page, or call goHome())
        nativeBack();
    }

    // ───────────────────────────── called from Rust ─────────────────────────────

    public void showIme() {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                ime.requestFocus();
                imm.restartInput(ime);
                imm.showSoftInput(ime, 0);
                // some devices need a second request once the window has taken focus
                ime.postDelayed(new Runnable() {
                    @Override
                    public void run() {
                        if (!ime.hasFocus()) ime.requestFocus();
                        imm.showSoftInput(ime, 0);
                    }
                }, 120);
            }
        });
    }

    public void hideIme() {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                hideImeNow();
            }
        });
    }

    private void hideImeNow() {
        if (imm != null && ime != null) {
            imm.hideSoftInputFromWindow(ime.getWindowToken(), 0);
        }
    }

    /** Dark app chrome → light system-bar icons, light chrome → dark icons. */
    @SuppressWarnings("deprecation")
    public void setDarkBars(final boolean dark) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                Window w = getWindow();
                if (Build.VERSION.SDK_INT >= 30) {
                    WindowInsetsController c = w.getInsetsController();
                    if (c != null) {
                        int m = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                                | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
                        c.setSystemBarsAppearance(dark ? 0 : m, m);
                    }
                } else {
                    View d = w.getDecorView();
                    int f = d.getSystemUiVisibility();
                    if (Build.VERSION.SDK_INT >= 23) {
                        f = dark ? (f & ~View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR) : (f | View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR);
                    }
                    if (Build.VERSION.SDK_INT >= 26) {
                        f = dark ? (f & ~View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR) : (f | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR);
                    }
                    d.setSystemUiVisibility(f);
                }
            }
        });
    }

    public void goHome() {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                moveTaskToBack(true);
            }
        });
    }

    public void openUrl(final String url) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    Intent i = new Intent(Intent.ACTION_VIEW, Uri.parse(url));
                    i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
                    startActivity(i);
                } catch (Exception e) {
                    Toast.makeText(LumenActivity.this, "No app can open this link", Toast.LENGTH_SHORT).show();
                }
            }
        });
    }

    @SuppressWarnings("deprecation")
    public void haptic(final int ms) {
        try {
            Vibrator v = (Vibrator) getSystemService(Context.VIBRATOR_SERVICE);
            if (v == null || !v.hasVibrator()) return;
            if (Build.VERSION.SDK_INT >= 26) {
                v.vibrate(VibrationEffect.createOneShot(Math.max(1, ms), VibrationEffect.DEFAULT_AMPLITUDE));
            } else {
                v.vibrate(ms);
            }
        } catch (Exception ignored) {
        }
    }

    public String getClipboardText() {
        try {
            ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            if (cm == null || !cm.hasPrimaryClip()) return "";
            ClipData d = cm.getPrimaryClip();
            if (d == null || d.getItemCount() == 0) return "";
            CharSequence t = d.getItemAt(0).coerceToText(this);
            return t == null ? "" : t.toString();
        } catch (Exception e) {
            return "";
        }
    }

    public void setClipboardText(final String text) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
                    if (cm != null) cm.setPrimaryClip(ClipData.newPlainText("Lumen", text));
                } catch (Exception ignored) {
                }
            }
        });
    }

    // ───────────────────────────── files (Storage Access Framework) ─────────────────────────────

    public void pickFile(final int req, final String mime) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    Intent i = new Intent(Intent.ACTION_OPEN_DOCUMENT);
                    i.addCategory(Intent.CATEGORY_OPENABLE);
                    i.setType(mime);
                    if (!"*/*".equals(mime)) {
                        i.putExtra(Intent.EXTRA_MIME_TYPES, new String[]{mime});
                    }
                    pendingPick = req;
                    startActivityForResult(i, REQ_OPEN);
                } catch (Exception e) {
                    pendingPick = -1;
                    nativePickCancelled(req);
                    Toast.makeText(LumenActivity.this, "No file manager available", Toast.LENGTH_SHORT).show();
                }
            }
        });
    }

    public void saveFile(final String name, final String mime, final byte[] data) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    pendingSaveName = name;
                    pendingSaveMime = mime;
                    pendingSaveData = data;
                    Intent i = new Intent(Intent.ACTION_CREATE_DOCUMENT);
                    i.addCategory(Intent.CATEGORY_OPENABLE);
                    i.setType(mime == null || mime.isEmpty() ? "application/octet-stream" : mime);
                    i.putExtra(Intent.EXTRA_TITLE, name);
                    startActivityForResult(i, REQ_SAVE);
                } catch (Exception e) {
                    pendingSaveData = null;
                    Toast.makeText(LumenActivity.this, "No file manager available", Toast.LENGTH_SHORT).show();
                }
            }
        });
    }

    @Override
    protected void onActivityResult(int requestCode, final int resultCode, final Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode == REQ_OPEN) {
            final int req = pendingPick;
            pendingPick = -1;
            if (resultCode != RESULT_OK || data == null || data.getData() == null) {
                nativePickCancelled(req);
                return;
            }
            final Uri uri = data.getData();
            new Thread(new Runnable() {
                @Override
                public void run() {
                    readPicked(req, uri);
                }
            }, "lumen-read").start();
        } else if (requestCode == REQ_SAVE) {
            final byte[] bytes = pendingSaveData;
            final String name = pendingSaveName;
            pendingSaveData = null;
            if (resultCode != RESULT_OK || data == null || data.getData() == null || bytes == null) {
                return;
            }
            final Uri uri = data.getData();
            new Thread(new Runnable() {
                @Override
                public void run() {
                    boolean ok = false;
                    try {
                        OutputStream out = getContentResolver().openOutputStream(uri, "wt");
                        if (out != null) {
                            out.write(bytes);
                            out.flush();
                            out.close();
                            ok = true;
                        }
                    } catch (Exception ignored) {
                    }
                    final String msg = ok ? "Saved " + name : "Could not save " + name;
                    runOnUiThread(new Runnable() {
                        @Override
                        public void run() {
                            Toast.makeText(LumenActivity.this, msg, Toast.LENGTH_SHORT).show();
                        }
                    });
                }
            }, "lumen-write").start();
        }
    }

    private void readPicked(int req, Uri uri) {
        String name = "file";
        String mime = "";
        try {
            mime = String.valueOf(getContentResolver().getType(uri));
            Cursor c = getContentResolver().query(uri, new String[]{OpenableColumns.DISPLAY_NAME}, null, null, null);
            if (c != null) {
                try {
                    if (c.moveToFirst()) {
                        int ix = c.getColumnIndex(OpenableColumns.DISPLAY_NAME);
                        if (ix >= 0 && c.getString(ix) != null) name = c.getString(ix);
                    }
                } finally {
                    c.close();
                }
            }
            InputStream in = getContentResolver().openInputStream(uri);
            if (in == null) throw new Exception("no stream");
            ByteArrayOutputStream buf = new ByteArrayOutputStream(64 * 1024);
            byte[] chunk = new byte[64 * 1024];
            long total = 0;
            int n;
            final long limit = 96L * 1024 * 1024;
            while ((n = in.read(chunk)) > 0) {
                total += n;
                if (total > limit) {
                    in.close();
                    toast("This file is too large (limit 96 MB)");
                    nativePickCancelled(req);
                    return;
                }
                buf.write(chunk, 0, n);
            }
            in.close();
            nativePicked(req, name, mime, buf.toByteArray());
        } catch (Throwable t) {
            toast("Could not read the selected file");
            nativePickCancelled(req);
        }
    }

    private void toast(final String msg) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                Toast.makeText(LumenActivity.this, msg, Toast.LENGTH_SHORT).show();
            }
        });
    }

    // ───────────────────────────── notifications ─────────────────────────────

    public void requestNotificationPermission() {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                if (Build.VERSION.SDK_INT >= 33
                        && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
                    requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS}, 7003);
                }
            }
        });
    }

    public void notifyUser(final String title, final String body, final String tag) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    if (Build.VERSION.SDK_INT >= 33
                            && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
                        requestPermissions(new String[]{Manifest.permission.POST_NOTIFICATIONS}, 7003);
                        return;
                    }
                    NotificationManager nm = (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
                    if (nm == null) return;
                    nm.createNotificationChannel(new NotificationChannel(CHANNEL, "Reminders", NotificationManager.IMPORTANCE_DEFAULT));
                    Intent open = new Intent(LumenActivity.this, LumenActivity.class);
                    open.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP);
                    PendingIntent pi = PendingIntent.getActivity(LumenActivity.this, 0, open,
                            PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
                    int icon = getResources().getIdentifier("ic_stat_lumen", "drawable", getPackageName());
                    if (icon == 0) icon = getApplicationInfo().icon;
                    android.app.Notification n = new android.app.Notification.Builder(LumenActivity.this, CHANNEL)
                            .setSmallIcon(icon)
                            .setContentTitle(title)
                            .setContentText(body)
                            .setStyle(new android.app.Notification.BigTextStyle().bigText(body))
                            .setContentIntent(pi)
                            .setAutoCancel(true)
                            .build();
                    nm.notify(tag == null ? "lumen" : tag, 1, n);
                } catch (Exception ignored) {
                }
            }
        });
    }

    // ───────────────────────────── printing ─────────────────────────────

    public void printHtml(final String name, final String html) {
        runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    printView = new WebView(LumenActivity.this);
                    printView.setWebViewClient(new WebViewClient() {
                        @Override
                        public void onPageFinished(WebView view, String url) {
                            PrintManager pm = (PrintManager) getSystemService(Context.PRINT_SERVICE);
                            if (pm != null) {
                                pm.print(name, view.createPrintDocumentAdapter(name), new PrintAttributes.Builder().build());
                            }
                        }
                    });
                    printView.loadDataWithBaseURL(null, html, "text/html", "UTF-8", null);
                } catch (Exception e) {
                    Toast.makeText(LumenActivity.this, "Printing is not available", Toast.LENGTH_SHORT).show();
                }
            }
        });
    }

    // ───────────────────────────── soft keyboard ─────────────────────────────

    /** An invisible text editor. Its InputConnection turns what the keyboard commits into events for egui. */
    static final class ImeView extends View {
        ImeView(Context c) {
            super(c);
            setFocusable(true);
            setFocusableInTouchMode(true);
            setAlpha(0f);
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo ei) {
            // VISIBLE_PASSWORD + NO_SUGGESTIONS: the keyboard commits every character immediately (no composing region we cannot show)
            ei.inputType = android.text.InputType.TYPE_CLASS_TEXT
                    | android.text.InputType.TYPE_TEXT_FLAG_MULTI_LINE
                    | android.text.InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS
                    | android.text.InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD;
            ei.imeOptions = EditorInfo.IME_FLAG_NO_FULLSCREEN
                    | EditorInfo.IME_FLAG_NO_EXTRACT_UI
                    | EditorInfo.IME_FLAG_NO_ENTER_ACTION
                    | EditorInfo.IME_ACTION_NONE;
            return new Conn(this);
        }
    }

    static final class Conn extends BaseInputConnection {
        private int composing = 0; // code points of the composing text already sent to Rust

        Conn(View v) {
            super(v, false);
        }

        private static void key(int code) {
            nativeKey(code, true);
            nativeKey(code, false);
        }

        private void replaceComposing(CharSequence t) {
            for (int i = 0; i < composing; i++) key(KeyEvent.KEYCODE_DEL);
            composing = 0;
            String s = t == null ? "" : t.toString();
            if (!s.isEmpty()) nativeText(s);
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            replaceComposing(text);
            return true;
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            String s = text == null ? "" : text.toString();
            replaceComposing(s);
            composing = s.codePointCount(0, s.length());
            return true;
        }

        @Override
        public boolean finishComposingText() {
            composing = 0;
            return true;
        }

        @Override
        public boolean deleteSurroundingText(int before, int after) {
            composing = 0;
            for (int i = 0; i < before; i++) key(KeyEvent.KEYCODE_DEL);
            for (int i = 0; i < after; i++) key(KeyEvent.KEYCODE_FORWARD_DEL);
            return true;
        }

        @Override
        public boolean deleteSurroundingTextInCodePoints(int before, int after) {
            return deleteSurroundingText(before, after);
        }

        @Override
        public boolean sendKeyEvent(KeyEvent e) {
            int code = e.getKeyCode();
            boolean down = e.getAction() == KeyEvent.ACTION_DOWN;
            switch (code) {
                case KeyEvent.KEYCODE_DEL:
                case KeyEvent.KEYCODE_FORWARD_DEL:
                case KeyEvent.KEYCODE_ENTER:
                case KeyEvent.KEYCODE_NUMPAD_ENTER:
                case KeyEvent.KEYCODE_TAB:
                case KeyEvent.KEYCODE_DPAD_LEFT:
                case KeyEvent.KEYCODE_DPAD_RIGHT:
                case KeyEvent.KEYCODE_DPAD_UP:
                case KeyEvent.KEYCODE_DPAD_DOWN:
                case KeyEvent.KEYCODE_MOVE_HOME:
                case KeyEvent.KEYCODE_MOVE_END:
                    composing = 0;
                    nativeKey(code, down);
                    return true;
                default:
                    if (down && e.getUnicodeChar() != 0 && !e.isCtrlPressed() && !e.isAltPressed()) {
                        composing = 0;
                        nativeText(new String(Character.toChars(e.getUnicodeChar())));
                    }
                    return true;
            }
        }

        @Override
        public boolean performEditorAction(int actionCode) {
            key(KeyEvent.KEYCODE_ENTER);
            return true;
        }

        @Override
        public boolean setSelection(int start, int end) {
            return true;
        }

        @Override
        public boolean setComposingRegion(int start, int end) {
            return true;
        }

        @Override
        public CharSequence getTextBeforeCursor(int n, int flags) {
            return "";
        }

        @Override
        public CharSequence getTextAfterCursor(int n, int flags) {
            return "";
        }

        @Override
        public CharSequence getSelectedText(int flags) {
            return null;
        }

        @Override
        public int getCursorCapsMode(int reqModes) {
            return 0;
        }

        @Override
        public boolean beginBatchEdit() {
            return true;
        }

        @Override
        public boolean endBatchEdit() {
            return true;
        }
    }
}
