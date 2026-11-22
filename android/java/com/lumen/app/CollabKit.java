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
    // The scanner is its own activity (ScanActivity): a NativeActivity cannot show Java views / a camera preview in its window.

    static void scanQr(final LumenActivity a) {
        a.runOnUiThread(new Runnable() {
            @Override
            public void run() {
                try {
                    a.startActivity(new android.content.Intent(a, ScanActivity.class));
                } catch (Throwable t) {
                    android.widget.Toast.makeText(a, "Camera scanner is not available", android.widget.Toast.LENGTH_SHORT).show();
                }
            }
        });
    }

    /** The permission is requested by ScanActivity itself; kept so LumenActivity's callback stays valid. */
    static void onPermission(LumenActivity a, int code, int[] results) {
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
