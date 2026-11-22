package com.lumen.app;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;

/**
 * Foreground service that exists only while a collaboration session runs (hosting or joined).
 *
 * Why: when the app is in the background Android freezes the process after a short while, and with it the sockets, the relay long-poll
 * and the hole-punched UDP path — a joined device would silently drop out and a host would stop answering. A foreground service
 * (type dataSync, visible notification, one tap to return) keeps the process, a partial CPU wake lock and a Wi-Fi lock alive, so
 * edits keep flowing while the screen is off. Nothing runs when no session is active; stopping the session stops the service.
 */
public class CollabService extends Service {
    static final String CHANNEL = "lumen-collab-session";
    static final int ID = 4610;
    private PowerManager.WakeLock cpu;
    private WifiManager.WifiLock wifi;

    static void start(Context c, String text) {
        Intent i = new Intent(c, CollabService.class).putExtra("text", text == null ? "" : text);
        try {
            if (Build.VERSION.SDK_INT >= 26) c.startForegroundService(i); else c.startService(i);
        } catch (Throwable t) {
            // background-start restrictions: the session still works while the app is on screen
        }
    }

    static void stop(Context c) {
        try {
            c.stopService(new Intent(c, CollabService.class));
        } catch (Throwable ignored) {
        }
    }

    @Override public IBinder onBind(Intent intent) {
        return null;
    }

    @Override public int onStartCommand(Intent intent, int flags, int startId) {
        String text = intent != null ? intent.getStringExtra("text") : null;
        if (text == null || text.isEmpty()) text = "Live collaboration is running";
        NotificationManager nm = (NotificationManager) getSystemService(Context.NOTIFICATION_SERVICE);
        if (Build.VERSION.SDK_INT >= 26 && nm != null) {
            NotificationChannel ch = new NotificationChannel(CHANNEL, "Collaboration session", NotificationManager.IMPORTANCE_LOW);
            ch.setDescription("Shown while Lumen keeps a shared session connected in the background");
            ch.setShowBadge(false);
            nm.createNotificationChannel(ch);
        }
        Intent open = new Intent(this, LumenActivity.class).addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP);
        PendingIntent pi = PendingIntent.getActivity(this, 0, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification.Builder b = Build.VERSION.SDK_INT >= 26 ? new Notification.Builder(this, CHANNEL) : new Notification.Builder(this);
        b.setContentTitle("Lumen · shared session").setContentText(text).setSmallIcon(android.R.drawable.stat_notify_sync)
                .setContentIntent(pi).setOngoing(true).setOnlyAlertOnce(true).setCategory(Notification.CATEGORY_SERVICE);
        Notification n = b.build();
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC);
        } else {
            startForeground(ID, n);
        }
        try {
            if (cpu == null) {
                PowerManager pm = (PowerManager) getSystemService(Context.POWER_SERVICE);
                cpu = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "lumen:collab");
                cpu.setReferenceCounted(false);
            }
            if (!cpu.isHeld()) cpu.acquire(4 * 60 * 60 * 1000L); // a session never needs the CPU for more than four hours without a restart
            if (wifi == null) {
                WifiManager wm = (WifiManager) getApplicationContext().getSystemService(Context.WIFI_SERVICE);
                wifi = wm.createWifiLock(WifiManager.WIFI_MODE_FULL_HIGH_PERF, "lumen:collab");
                wifi.setReferenceCounted(false);
            }
            if (!wifi.isHeld()) wifi.acquire();
        } catch (Throwable ignored) {
        }
        return START_NOT_STICKY; // a killed process has no session to resume
    }

    @Override public void onDestroy() {
        try {
            if (cpu != null && cpu.isHeld()) cpu.release();
            if (wifi != null && wifi.isHeld()) wifi.release();
        } catch (Throwable ignored) {
        }
        super.onDestroy();
    }
}
