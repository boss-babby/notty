package com.lumen.app;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.graphics.Color;
import android.graphics.Typeface;
import android.hardware.Camera;
import android.net.nsd.NsdManager;
import android.net.nsd.NsdServiceInfo;
import android.view.Gravity;
import android.view.SurfaceHolder;
import android.view.SurfaceView;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowManager;
import android.widget.FrameLayout;
import android.widget.TextView;

import java.nio.charset.Charset;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ArrayBlockingQueue;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.ThreadPoolExecutor;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;
import java.util.LinkedList;

/**
 * Local collaboration helpers that only the Android framework offers:
 *  - a camera QR scanner (frames go to Rust, which decodes them with a pure-Rust QR reader and opens the join dialog), and
 *  - NsdManager (DNS-SD) registration + discovery of "_lumen._tcp" hosts, for Wi-Fi networks that filter UDP broadcast.
 * No AndroidX / Play Services: plain android.hardware.Camera and android.net.nsd.
 */
final class CollabKit {
    private CollabKit() {}

    static final int REQ_CAMERA = 7010;
    private static final Charset UTF8 = Charset.forName("UTF-8");

    /** Rust: decodes a QR code from the luma plane of a preview frame; true = found and queued as a join link. */
    static native boolean nativeScanFrame(byte[] luma, int w, int h);
    /** Rust: a host was resolved through NSD. {@code info} is the beacon JSON the host published. */
    static native void nativeNsdFound(String name, String host, int port, String info);

    // ───────────────────────────── QR scanner ─────────────────────────────

    private static FrameLayout overlay;
    private static Camera camera;
    private static final AtomicBoolean busy = new AtomicBoolean(false);
    private static long lastFrame = 0;
    private static ExecutorService decoder;

    static void scanQr(final LumenActivity a) {
        a.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                if (a.checkSelfPermission(Manifest.permission.CAMERA) != PackageManager.PERMISSION_GRANTED) {
                    a.requestPermissions(new String[]{Manifest.permission.CAMERA}, REQ_CAMERA);
                    return;
                }
                startScan(a);
            }
        });
    }

    static void onPermission(LumenActivity a, int code, int[] results) {
        if (code != REQ_CAMERA) return;
        if (results != null && results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            startScan(a);
        } else {
            android.widget.Toast.makeText(a, "Camera permission is needed to scan the QR code. You can also type the code.", android.widget.Toast.LENGTH_LONG).show();
        }
    }

    private static void startScan(final LumenActivity a) {
        if (overlay != null) return;
        final FrameLayout root = new FrameLayout(a);
        root.setBackgroundColor(Color.BLACK);
        SurfaceView sv = new SurfaceView(a);
        root.addView(sv, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        TextView hint = new TextView(a);
        hint.setText("Point the camera at the QR code on the host's screen");
        hint.setTextColor(Color.WHITE);
        hint.setTextSize(15);
        hint.setTypeface(Typeface.DEFAULT_BOLD);
        hint.setGravity(Gravity.CENTER);
        hint.setBackgroundColor(0x99000000);
        int pad = (int) (16 * a.getResources().getDisplayMetrics().density);
        hint.setPadding(pad, pad * 2, pad, pad);
        root.addView(hint, new FrameLayout.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.TOP));
        TextView close = new TextView(a);
        close.setText("Cancel");
        close.setTextColor(Color.WHITE);
        close.setTextSize(16);
        close.setTypeface(Typeface.DEFAULT_BOLD);
        close.setGravity(Gravity.CENTER);
        close.setBackgroundColor(0xCC2383E2);
        close.setPadding(pad * 2, pad, pad * 2, pad);
        FrameLayout.LayoutParams lp = new FrameLayout.LayoutParams(ViewGroup.LayoutParams.WRAP_CONTENT, ViewGroup.LayoutParams.WRAP_CONTENT, Gravity.BOTTOM | Gravity.CENTER_HORIZONTAL);
        lp.bottomMargin = pad * 3;
        root.addView(close, lp);
        close.setOnClickListener(new View.OnClickListener() {
            @Override
            public void onClick(View v) {
                stopScan(a);
            }
        });
        overlay = root;
        a.addContentView(root, new ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT));
        decoder = new ThreadPoolExecutor(1, 1, 0L, TimeUnit.MILLISECONDS, new ArrayBlockingQueue<Runnable>(1), new ThreadPoolExecutor.DiscardPolicy());
        sv.getHolder().addCallback(new SurfaceHolder.Callback() {
            @Override
            public void surfaceCreated(SurfaceHolder holder) {
                openCamera(a, holder);
            }

            @Override
            public void surfaceChanged(SurfaceHolder holder, int format, int width, int height) {}

            @Override
            public void surfaceDestroyed(SurfaceHolder holder) {
                releaseCamera();
            }
        });
    }

    private static void openCamera(final LumenActivity a, SurfaceHolder holder) {
        try {
            camera = Camera.open();
            Camera.Parameters p = camera.getParameters();
            List<Camera.Size> sizes = p.getSupportedPreviewSizes();
            Camera.Size best = null;
            for (Camera.Size s : sizes) {
                // enough pixels for a QR code on a phone screen, few enough to decode several times per second
                if (best == null || Math.abs(s.width - 1280) < Math.abs(best.width - 1280)) best = s;
            }
            if (best != null) p.setPreviewSize(best.width, best.height);
            List<String> modes = p.getSupportedFocusModes();
            if (modes != null && modes.contains(Camera.Parameters.FOCUS_MODE_CONTINUOUS_PICTURE)) {
                p.setFocusMode(Camera.Parameters.FOCUS_MODE_CONTINUOUS_PICTURE);
            }
            camera.setParameters(p);
            Camera.CameraInfo info = new Camera.CameraInfo();
            Camera.getCameraInfo(0, info);
            int rot = ((WindowManager) a.getSystemService(Context.WINDOW_SERVICE)).getDefaultDisplay().getRotation() * 90;
            camera.setDisplayOrientation((info.orientation - rot + 360) % 360);
            camera.setPreviewDisplay(holder);
            camera.setPreviewCallback(new Camera.PreviewCallback() {
                @Override
                public void onPreviewFrame(final byte[] data, Camera c) {
                    long now = System.currentTimeMillis();
                    if (now - lastFrame < 200 || data == null || busy.get()) return;
                    lastFrame = now;
                    final Camera.Size s = c.getParameters().getPreviewSize();
                    final byte[] copy = data.clone();
                    busy.set(true);
                    decoder.execute(new Runnable() {
                        @Override
                        public void run() {
                            boolean found = false;
                            try {
                                found = nativeScanFrame(copy, s.width, s.height);
                            } catch (Throwable ignored) {
                            }
                            busy.set(false);
                            if (found) {
                                a.runOnUiThread(new Runnable() {
                                    @Override
                                    public void run() {
                                        stopScan(a);
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
            android.widget.Toast.makeText(a, "Camera is not available", android.widget.Toast.LENGTH_SHORT).show();
            stopScan(a);
        }
    }

    private static void releaseCamera() {
        if (camera != null) {
            try {
                camera.setPreviewCallback(null);
                camera.stopPreview();
            } catch (Throwable ignored) {
            }
            try {
                camera.release();
            } catch (Throwable ignored) {
            }
            camera = null;
        }
    }

    static void stopScan(LumenActivity a) {
        releaseCamera();
        if (decoder != null) {
            decoder.shutdownNow();
            decoder = null;
        }
        if (overlay != null) {
            ViewGroup parent = (ViewGroup) overlay.getParent();
            if (parent != null) parent.removeView(overlay);
            overlay = null;
        }
    }

    static boolean scanning() {
        return overlay != null;
    }

    // ───────────────────────────── NSD (DNS-SD) ─────────────────────────────

    private static NsdManager nsd;
    private static NsdManager.RegistrationListener registration;
    private static NsdManager.DiscoveryListener discovery;
    private static final LinkedList<NsdServiceInfo> toResolve = new LinkedList<NsdServiceInfo>();
    private static boolean resolving = false;
    private static final String TYPE = "_lumen._tcp.";

    private static NsdManager mgr(Context c) {
        if (nsd == null) nsd = (NsdManager) c.getSystemService(Context.NSD_SERVICE);
        return nsd;
    }

    /** Publishes this device as a host. {@code info} = beacon JSON (split into TXT attributes of ≤ 180 bytes). */
    static void nsdStart(final Context c, String name, int port, String info) {
        nsdStop();
        try {
            NsdServiceInfo si = new NsdServiceInfo();
            si.setServiceName("lumen-" + name);
            si.setServiceType(TYPE);
            si.setPort(port);
            byte[] raw = info.getBytes(UTF8);
            int n = 0;
            for (int i = 0; i < raw.length && n < 6; i += 180, n++) {
                si.setAttribute("i" + n, new String(raw, i, Math.min(180, raw.length - i), UTF8));
            }
            registration = new NsdManager.RegistrationListener() {
                @Override public void onServiceRegistered(NsdServiceInfo s) {}
                @Override public void onRegistrationFailed(NsdServiceInfo s, int e) { registration = null; }
                @Override public void onServiceUnregistered(NsdServiceInfo s) {}
                @Override public void onUnregistrationFailed(NsdServiceInfo s, int e) {}
            };
            mgr(c).registerService(si, NsdManager.PROTOCOL_DNS_SD, registration);
        } catch (Throwable t) {
            registration = null;
        }
    }

    static void nsdStop() {
        if (registration != null && nsd != null) {
            try { nsd.unregisterService(registration); } catch (Throwable ignored) {}
        }
        registration = null;
    }

    static void nsdDiscover(final Context c, boolean on) {
        if (!on) {
            if (discovery != null && nsd != null) {
                try { nsd.stopServiceDiscovery(discovery); } catch (Throwable ignored) {}
            }
            discovery = null;
            synchronized (toResolve) { toResolve.clear(); resolving = false; }
            return;
        }
        if (discovery != null) return;
        try {
            discovery = new NsdManager.DiscoveryListener() {
                @Override public void onStartDiscoveryFailed(String t, int e) { discovery = null; }
                @Override public void onStopDiscoveryFailed(String t, int e) {}
                @Override public void onDiscoveryStarted(String t) {}
                @Override public void onDiscoveryStopped(String t) {}
                @Override public void onServiceLost(NsdServiceInfo s) {}
                @Override
                public void onServiceFound(NsdServiceInfo s) {
                    if (s.getServiceName() == null || !s.getServiceName().startsWith("lumen-")) return;
                    synchronized (toResolve) { toResolve.add(s); }
                    resolveNext(c);
                }
            };
            mgr(c).discoverServices(TYPE, NsdManager.PROTOCOL_DNS_SD, discovery);
        } catch (Throwable t) {
            discovery = null;
        }
    }

    /** Pre-Android-14 NsdManager resolves one service at a time. */
    private static void resolveNext(final Context c) {
        final NsdServiceInfo s;
        synchronized (toResolve) {
            if (resolving || toResolve.isEmpty()) return;
            s = toResolve.removeFirst();
            resolving = true;
        }
        try {
            mgr(c).resolveService(s, new NsdManager.ResolveListener() {
                @Override
                public void onResolveFailed(NsdServiceInfo si, int e) {
                    synchronized (toResolve) { resolving = false; }
                    resolveNext(c);
                }

                @Override
                public void onServiceResolved(NsdServiceInfo si) {
                    try {
                        StringBuilder info = new StringBuilder();
                        Map<String, byte[]> attrs = si.getAttributes();
                        for (int i = 0; i < 6; i++) {
                            byte[] v = attrs.get("i" + i);
                            if (v == null) break;
                            info.append(new String(v, UTF8));
                        }
                        if (si.getHost() != null && info.length() > 0) {
                            nativeNsdFound(si.getServiceName(), si.getHost().getHostAddress(), si.getPort(), info.toString());
                        }
                    } catch (Throwable ignored) {
                    }
                    synchronized (toResolve) { resolving = false; }
                    resolveNext(c);
                }
            });
        } catch (Throwable t) {
            synchronized (toResolve) { resolving = false; }
        }
    }
}
