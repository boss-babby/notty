package com.lumen.app;

import android.Manifest;
import android.content.Context;
import android.content.pm.PackageManager;
import android.net.ConnectivityManager;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.net.wifi.WifiConfiguration;
import android.net.wifi.WifiManager;
import android.net.wifi.WifiNetworkSpecifier;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;

import java.net.Inet4Address;
import java.net.InetAddress;
import java.net.NetworkInterface;
import java.security.KeyStore;
import java.util.Collections;

import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/**
 * "AirDrop-like" pairing for two phones that are NOT on the same Wi-Fi, plus the device-key vault.
 *
 *  - The host opens a LocalOnlyHotspot (a private, internet-less access point that Android allows apps to create), and shows its
 *    SSID / passphrase together with the Lumen join link in one QR code.
 *  - The joiner scans that code in the app; {@link #wifiJoin} connects through a WifiNetworkSpecifier (no system settings screen) and
 *    binds the process to that network so Lumen's sockets use it; Rust then dials the host's addresses.
 *  - {@link #keyWrap} / {@link #keyUnwrap} seal the device's Noise key with a non-exportable AES-GCM key of the Android Keystore.
 *
 * State is exposed as small pipe-separated strings that Rust polls once a frame (no callbacks into native code are needed).
 */
final class NearbyKit {
    private NearbyKit() {}

    static final int REQ_NEARBY = 7020;
    private static final String KEY_ALIAS = "lumen_device_key_v1";

    /** "idle||", "perm||", "starting||", "up|SSID|PASS", "failed|reason|". */
    private static volatile String hotspot = "idle||";
    /** "idle|", "joining|", "up|", "failed|reason". */
    private static volatile String join = "idle|";
    private static WifiManager.LocalOnlyHotspotReservation reservation;
    private static ConnectivityManager.NetworkCallback joinCallback;

    static String hotspot() {
        return hotspot;
    }

    static String join() {
        return join;
    }

    private static String needed() {
        return Build.VERSION.SDK_INT >= 33 ? Manifest.permission.NEARBY_WIFI_DEVICES : Manifest.permission.ACCESS_FINE_LOCATION;
    }

    static void onPermission(LumenActivity a, int code, int[] results) {
        if (code != REQ_NEARBY) return;
        if (results.length > 0 && results[0] == PackageManager.PERMISSION_GRANTED) {
            hotspotStart(a);
        } else {
            hotspot = "failed|Permission denied — Android needs it to create a Wi-Fi hotspot|";
        }
    }

    // ───────────────────────────── host: local-only hotspot ─────────────────────────────

    @SuppressWarnings("deprecation")
    static void hotspotStart(final LumenActivity a) {
        try {
            if (a.checkSelfPermission(needed()) != PackageManager.PERMISSION_GRANTED) {
                hotspot = "perm||";
                a.requestPermissions(new String[] {needed()}, REQ_NEARBY);
                return;
            }
            if (reservation != null) return; // already up
            final WifiManager wm = (WifiManager) a.getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            if (wm == null) {
                hotspot = "failed|This device has no Wi-Fi|";
                return;
            }
            hotspot = "starting||";
            wm.startLocalOnlyHotspot(new WifiManager.LocalOnlyHotspotCallback() {
                @Override
                public void onStarted(WifiManager.LocalOnlyHotspotReservation r) {
                    reservation = r;
                    String ssid = "";
                    String pass = "";
                    try {
                        if (Build.VERSION.SDK_INT >= 30 && r.getSoftApConfiguration() != null) {
                            ssid = String.valueOf(r.getSoftApConfiguration().getSsid());
                            pass = String.valueOf(r.getSoftApConfiguration().getPassphrase());
                        } else {
                            WifiConfiguration c = r.getWifiConfiguration();
                            if (c != null) {
                                ssid = c.SSID == null ? "" : c.SSID.replace("\"", "");
                                pass = c.preSharedKey == null ? "" : c.preSharedKey.replace("\"", "");
                            }
                        }
                    } catch (Throwable ignored) {
                    }
                    hotspot = "up|" + ssid.replace("|", "") + "|" + pass.replace("|", "");
                }

                @Override
                public void onStopped() {
                    reservation = null;
                    hotspot = "idle||";
                }

                @Override
                public void onFailed(int reason) {
                    reservation = null;
                    String why = reason == ERROR_INCOMPATIBLE_MODE ? "Turn off Wi-Fi tethering first" : reason == ERROR_TETHERING_DISALLOWED ? "Hotspots are disabled on this device" : reason == ERROR_NO_CHANNEL ? "No free Wi-Fi channel" : "The hotspot could not start";
                    hotspot = "failed|" + why + "|";
                }
            }, new Handler(Looper.getMainLooper()));
        } catch (Throwable t) {
            hotspot = "failed|" + String.valueOf(t.getMessage()).replace("|", " ") + "|";
        }
    }

    static void hotspotStop() {
        try {
            if (reservation != null) reservation.close();
        } catch (Throwable ignored) {
        }
        reservation = null;
        hotspot = "idle||";
    }

    // ───────────────────────────── joiner: connect to that hotspot ─────────────────────────────

    static void wifiJoin(final LumenActivity a, String ssid, String pass) {
        wifiLeave(a);
        if (Build.VERSION.SDK_INT < 29) {
            join = "failed|Joining a Wi-Fi network from the app needs Android 10 — join it in Settings, then scan again";
            return;
        }
        try {
            final ConnectivityManager cm = (ConnectivityManager) a.getSystemService(Context.CONNECTIVITY_SERVICE);
            WifiNetworkSpecifier.Builder sb = new WifiNetworkSpecifier.Builder().setSsid(ssid);
            if (pass != null && !pass.isEmpty()) sb.setWpa2Passphrase(pass);
            NetworkRequest req = new NetworkRequest.Builder().addTransportType(NetworkCapabilities.TRANSPORT_WIFI).removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET).setNetworkSpecifier(sb.build()).build();
            join = "joining|";
            joinCallback = new ConnectivityManager.NetworkCallback() {
                @Override
                public void onAvailable(Network network) {
                    cm.bindProcessToNetwork(network);
                    join = "up|";
                }

                @Override
                public void onUnavailable() {
                    join = "failed|Could not join that Wi-Fi network (declined or out of range)";
                }

                @Override
                public void onLost(Network network) {
                    cm.bindProcessToNetwork(null);
                    join = "idle|";
                }
            };
            cm.requestNetwork(req, joinCallback, 45000);
        } catch (Throwable t) {
            join = "failed|" + String.valueOf(t.getMessage()).replace("|", " ");
        }
    }

    static void wifiLeave(LumenActivity a) {
        try {
            ConnectivityManager cm = (ConnectivityManager) a.getSystemService(Context.CONNECTIVITY_SERVICE);
            if (joinCallback != null) cm.unregisterNetworkCallback(joinCallback);
            if (Build.VERSION.SDK_INT >= 23) cm.bindProcessToNetwork(null);
        } catch (Throwable ignored) {
        }
        joinCallback = null;
        join = "idle|";
    }

    /** Every IPv4 address of every up interface (Wi-Fi, hotspot, Ethernet, USB tether), comma separated. */
    static String localIps() {
        StringBuilder sb = new StringBuilder();
        try {
            for (NetworkInterface ni : Collections.list(NetworkInterface.getNetworkInterfaces())) {
                if (!ni.isUp() || ni.isLoopback()) continue;
                String n = ni.getName();
                if (n.startsWith("rmnet") || n.startsWith("ccmni") || n.startsWith("tun") || n.startsWith("dummy")) continue; // cellular / VPN
                for (InetAddress ia : Collections.list(ni.getInetAddresses())) {
                    if (ia instanceof Inet4Address && !ia.isLoopbackAddress() && !ia.isLinkLocalAddress()) {
                        if (sb.length() > 0) sb.append(',');
                        sb.append(ia.getHostAddress());
                    }
                }
            }
        } catch (Throwable ignored) {
        }
        return sb.toString();
    }

    // ───────────────────────────── Keystore vault for the device key ─────────────────────────────

    private static SecretKey vaultKey() throws Exception {
        KeyStore ks = KeyStore.getInstance("AndroidKeyStore");
        ks.load(null);
        if (!ks.containsAlias(KEY_ALIAS)) {
            KeyGenerator kg = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
            kg.init(new KeyGenParameterSpec.Builder(KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT).setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).setKeySize(256).build());
            return kg.generateKey();
        }
        return (SecretKey) ks.getKey(KEY_ALIAS, null);
    }

    /** iv(12) ‖ ciphertext+tag; null when the Keystore is unavailable (the caller then keeps the key as it is). */
    static byte[] keyWrap(byte[] plain) {
        try {
            Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
            c.init(Cipher.ENCRYPT_MODE, vaultKey());
            byte[] iv = c.getIV();
            byte[] ct = c.doFinal(plain);
            byte[] out = new byte[iv.length + ct.length];
            System.arraycopy(iv, 0, out, 0, iv.length);
            System.arraycopy(ct, 0, out, iv.length, ct.length);
            return out;
        } catch (Throwable t) {
            return null;
        }
    }

    static byte[] keyUnwrap(byte[] blob) {
        try {
            if (blob == null || blob.length < 13 + 16) return null;
            Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
            c.init(Cipher.DECRYPT_MODE, vaultKey(), new GCMParameterSpec(128, blob, 0, 12));
            return c.doFinal(blob, 12, blob.length - 12);
        } catch (Throwable t) {
            return null;
        }
    }
}
