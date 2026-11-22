package com.lumen.app;

import android.Manifest;
import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Matrix;
import android.graphics.Paint;
import android.graphics.Path;
import android.graphics.RectF;
import android.graphics.SurfaceTexture;
import android.graphics.Typeface;
import android.hardware.Camera;
import android.os.Bundle;
import android.os.Vibrator;
import android.view.Gravity;
import android.view.Surface;
import android.view.TextureView;
import android.view.View;
import android.view.ViewGroup;
import android.view.Window;
import android.view.WindowManager;
import android.widget.FrameLayout;
import android.widget.TextView;
import android.widget.Toast;

import java.util.List;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/**
 * Full-screen QR scanner in its OWN activity window.
 *
 * Why an activity and not a view on top of LumenActivity: LumenActivity is a NativeActivity — the native code takes the window's
 * surface (egui draws into it), so Java views added to that window are never composited, and a SurfaceView's "hole" ends up behind
 * the opaque egui surface (the old "camera opens behind the whole app" bug). A separate activity has a normal view hierarchy.
 *
 * Preview: TextureView (always composited in view order, no z-order games). Frames: NV21 luma → Rust (`rqrr`) on one background
 * thread, at most ~7 per second, never queued up (a slow decode simply skips frames). Torch toggle, viewfinder, tap-to-focus.
 */
public class ScanActivity extends Activity implements TextureView.SurfaceTextureListener {
    private static final int REQ_CAMERA = 7011;

    private TextureView tv;
    private Camera camera;
    private Camera.Size previewSize;
    private int cameraId = 0;
    private boolean torch = false;
    private TextView torchBtn;
    private final AtomicBoolean busy = new AtomicBoolean(false);
    private volatile boolean done = false;
    private long lastFrame = 0;
    private ExecutorService decoder;

    @Override
    protected void onCreate(Bundle b) {
        super.onCreate(b);
        requestWindowFeature(Window.FEATURE_NO_TITLE);
        getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON | WindowManager.LayoutParams.FLAG_FULLSCREEN);
        final float d = getResources().getDisplayMetrics().density;
        final int pad = (int) (16 * d);

        FrameLayout root = new FrameLayout(this);
        root.setBackgroundColor(Color.BLACK);
        tv = new TextureView(this);
        tv.setSurfaceTextureListener(this);
        root.addView(tv, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        root.addView(new Finder(this), new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));

        TextView hint = new TextView(this);
        hint.setText("Scan the QR code shown by the host");
        hint.setTextColor(Color.WHITE);
        hint.setTextSize(16);
        hint.setTypeface(Typeface.DEFAULT_BOLD);
        hint.setGravity(Gravity.CENTER);
        hint.setPadding(pad, pad * 3, pad, pad);
        hint.setBackgroundColor(0x88000000);
        root.addView(hint, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP));

        TextView cancel = button("Cancel", 0xCC2383E2, pad);
        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM | Gravity.CENTER_HORIZONTAL);
        lp.bottomMargin = pad * 3;
        root.addView(cancel, lp);
        cancel.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                finish();
            }
        });
        torchBtn = button("Torch", 0x99000000, pad);
        FrameLayout.LayoutParams tp = new FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM | Gravity.END);
        tp.bottomMargin = pad * 3;
        tp.rightMargin = pad;
        root.addView(torchBtn, tp);
        torchBtn.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                setTorch(!torch);
            }
        });
        tv.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                focusNow();
            }
        });
        setContentView(root);
        immersive();
        if (checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(new String[]{Manifest.permission.CAMERA}, REQ_CAMERA);
        }
    }

    private TextView button(String label, int bg, int pad) {
        TextView t = new TextView(this);
        t.setText(label);
        t.setTextColor(Color.WHITE);
        t.setTextSize(16);
        t.setTypeface(Typeface.DEFAULT_BOLD);
        t.setGravity(Gravity.CENTER);
        t.setBackgroundColor(bg);
        t.setPadding(pad * 2, pad, pad * 2, pad);
        t.setMinimumWidth((int) (96 * getResources().getDisplayMetrics().density));
        return t;
    }

    private void immersive() {
        getWindow().getDecorView().setSystemUiVisibility(View.SYSTEM_UI_FLAG_LAYOUT_STABLE | View.SYSTEM_UI_FLAG_FULLSCREEN | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY);
    }

    @Override
    public void onRequestPermissionsResult(int code, String[] permissions, int[] results) {
        super.onRequestPermissionsResult(code, permissions, results);
        if (code != REQ_CAMERA) return;
        if (results != null && results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            if (tv.isAvailable()) openCamera(tv.getSurfaceTexture());
        } else {
            Toast.makeText(this, "Camera permission is needed to scan. You can also type the code.", Toast.LENGTH_LONG).show();
            finish();
        }
    }

    @Override
    protected void onResume() {
        super.onResume();
        immersive();
        if (tv != null && tv.isAvailable() && camera == null && checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) {
            openCamera(tv.getSurfaceTexture());
        }
    }

    @Override
    protected void onPause() {
        releaseCamera();
        super.onPause();
    }

    @Override
    protected void onDestroy() {
        releaseCamera();
        if (decoder != null) decoder.shutdownNow();
        super.onDestroy();
    }

    // ── TextureView ──
    @Override
    public void onSurfaceTextureAvailable(SurfaceTexture st, int w, int h) {
        if (checkSelfPermission(Manifest.permission.CAMERA) == PackageManager.PERMISSION_GRANTED) openCamera(st);
    }

    @Override
    public void onSurfaceTextureSizeChanged(SurfaceTexture st, int w, int h) {
        applyOrientation();
    }

    @Override
    public boolean onSurfaceTextureDestroyed(SurfaceTexture st) {
        releaseCamera();
        return true;
    }

    @Override
    public void onSurfaceTextureUpdated(SurfaceTexture st) {}

    // ── camera ──
    private void openCamera(SurfaceTexture st) {
        if (camera != null || done) return;
        try {
            Camera.CameraInfo info = new Camera.CameraInfo();
            cameraId = 0;
            for (int i = 0; i < Camera.getNumberOfCameras(); i++) {
                Camera.getCameraInfo(i, info);
                if (info.facing == Camera.CameraInfo.CAMERA_FACING_BACK) {
                    cameraId = i;
                    break;
                }
            }
            camera = Camera.open(cameraId);
            Camera.Parameters p = camera.getParameters();
            previewSize = pickSize(p.getSupportedPreviewSizes());
            if (previewSize != null) p.setPreviewSize(previewSize.width, previewSize.height);
            List<String> modes = p.getSupportedFocusModes();
            if (modes != null && modes.contains(Camera.Parameters.FOCUS_MODE_CONTINUOUS_PICTURE)) {
                p.setFocusMode(Camera.Parameters.FOCUS_MODE_CONTINUOUS_PICTURE);
            }
            camera.setParameters(p);
            camera.setPreviewTexture(st);
            applyOrientation();
            if (decoder == null) {
                decoder = new ThreadPoolExecutor(1, 1, 0L, TimeUnit.MILLISECONDS, new ArrayBlockingQueue<Runnable>(1), new ThreadPoolExecutor.DiscardPolicy());
            }
            camera.setPreviewCallback(new Camera.PreviewCallback() {
                @Override
                public void onPreviewFrame(final byte[] data, Camera c) {
                    long now = System.currentTimeMillis();
                    if (done || data == null || now - lastFrame < 140 || busy.get() || previewSize == null) return;
                    lastFrame = now;
                    final int w = previewSize.width, h = previewSize.height;
                    final byte[] luma = new byte[Math.min(data.length, w * h)];
                    System.arraycopy(data, 0, luma, 0, luma.length);
                    busy.set(true);
                    decoder.execute(new Runnable() {
                        @Override
                        public void run() {
                            boolean found = false;
                            try {
                                found = CollabKit.nativeScanFrame(luma, w, h);
                            } catch (Throwable ignored) {
                            }
                            busy.set(false);
                            if (found && !done) {
                                done = true;
                                runOnUiThread(new Runnable() {
                                    @Override
                                    public void run() {
                                        try {
                                            Vibrator v = (Vibrator) getSystemService(Context.VIBRATOR_SERVICE);
                                            if (v != null) v.vibrate(40);
                                        } catch (Throwable ignored) {
                                        }
                                        finish();
                                    }
                                });
                            }
                        }
                    });
                }
            });
            camera.startPreview();
        } catch (Throwable t) {
            releaseCamera();
            Toast.makeText(this, "Camera is not available", Toast.LENGTH_SHORT).show();
            finish();
        }
    }

    /** ~1280 px wide, aspect close to the screen (rotated), so the preview needs little cropping and decodes fast. */
    private Camera.Size pickSize(List<Camera.Size> sizes) {
        if (sizes == null || sizes.isEmpty()) return null;
        int vw = Math.max(1, tv.getWidth()), vh = Math.max(1, tv.getHeight());
        double want = Math.max(vw, vh) / (double) Math.min(vw, vh);
        Camera.Size best = null;
        double bestScore = Double.MAX_VALUE;
        for (Camera.Size s : sizes) {
            double ar = Math.max(s.width, s.height) / (double) Math.min(s.width, s.height);
            double score = Math.abs(ar - want) * 4000 + Math.abs(Math.max(s.width, s.height) - 1280);
            if (score < bestScore) {
                bestScore = score;
                best = s;
            }
        }
        return best;
    }

    private void applyOrientation() {
        if (camera == null || previewSize == null) return;
        try {
            Camera.CameraInfo info = new Camera.CameraInfo();
            Camera.getCameraInfo(cameraId, info);
            int rot = getWindowManager().getDefaultDisplay().getRotation();
            int deg = rot == Surface.ROTATION_90 ? 90 : rot == Surface.ROTATION_180 ? 180 : rot == Surface.ROTATION_270 ? 270 : 0;
            int result = (info.facing == Camera.CameraInfo.CAMERA_FACING_FRONT) ? (360 - (info.orientation + deg) % 360) % 360 : (info.orientation - deg + 360) % 360;
            camera.setDisplayOrientation(result);
            // centre-crop: the TextureView stretches the preview to its bounds, so undo the stretch and scale to fill
            boolean swap = result == 90 || result == 270;
            float pw = swap ? previewSize.height : previewSize.width, ph = swap ? previewSize.width : previewSize.height;
            float vw = tv.getWidth(), vh = tv.getHeight();
            if (vw <= 0 || vh <= 0) return;
            float scale = Math.max(vw / pw, vh / ph);
            Matrix m = new Matrix();
            m.setScale(pw * scale / vw, ph * scale / vh, vw / 2f, vh / 2f);
            tv.setTransform(m);
        } catch (Throwable ignored) {
        }
    }

    private void setTorch(boolean on) {
        if (camera == null) return;
        try {
            Camera.Parameters p = camera.getParameters();
            List<String> modes = p.getSupportedFlashModes();
            if (modes == null || !modes.contains(Camera.Parameters.FLASH_MODE_TORCH)) {
                Toast.makeText(this, "No torch on this device", Toast.LENGTH_SHORT).show();
                return;
            }
            p.setFlashMode(on ? Camera.Parameters.FLASH_MODE_TORCH : Camera.Parameters.FLASH_MODE_OFF);
            camera.setParameters(p);
            torch = on;
            torchBtn.setBackgroundColor(on ? 0xCCF5A623 : 0x99000000);
        } catch (Throwable ignored) {
        }
    }

    private void focusNow() {
        if (camera == null) return;
        try {
            camera.autoFocus(new Camera.AutoFocusCallback() {
                @Override
                public void onAutoFocus(boolean ok, Camera c) {}
            });
        } catch (Throwable ignored) {
        }
    }

    private void releaseCamera() {
        Camera c = camera;
        camera = null;
        torch = false;
        if (c != null) {
            try {
                c.setPreviewCallback(null);
                c.stopPreview();
            } catch (Throwable ignored) {
            }
            try {
                c.release();
            } catch (Throwable ignored) {
            }
        }
    }

    /** Dimmed surround with a clear rounded square and corner marks. */
    private static final class Finder extends View {
        private final Paint dim = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Paint mark = new Paint(Paint.ANTI_ALIAS_FLAG);
        private final Path hole = new Path();
        private final Path outer = new Path();
        private final RectF box = new RectF();

        Finder(Context c) {
            super(c);
            dim.setColor(0x99000000);
            mark.setColor(0xFF2383E2);
            mark.setStyle(Paint.Style.STROKE);
            mark.setStrokeWidth(5 * c.getResources().getDisplayMetrics().density);
            mark.setStrokeCap(Paint.Cap.ROUND);
            setClickable(false);
        }

        @Override
        protected void onDraw(Canvas cv) {
            float w = getWidth(), h = getHeight();
            float s = Math.min(w, h) * 0.68f;
            box.set((w - s) / 2f, (h - s) / 2f, (w + s) / 2f, (h + s) / 2f);
            float r = s * 0.06f;
            outer.reset();
            outer.addRect(0, 0, w, h, Path.Direction.CW);
            hole.reset();
            hole.addRoundRect(box, r, r, Path.Direction.CCW);
            outer.op(hole, Path.Op.DIFFERENCE);
            cv.drawPath(outer, dim);
            float k = s * 0.16f;
            cv.drawLines(new float[]{
                    box.left, box.top + k, box.left, box.top, box.left, box.top, box.left + k, box.top,
                    box.right - k, box.top, box.right, box.top, box.right, box.top, box.right, box.top + k,
                    box.left, box.bottom - k, box.left, box.bottom, box.left, box.bottom, box.left + k, box.bottom,
                    box.right - k, box.bottom, box.right, box.bottom, box.right, box.bottom, box.right, box.bottom - k}, mark);
        }
    }
}
