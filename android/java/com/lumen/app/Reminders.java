package com.lumen.app;

import android.Manifest;
import android.app.AlarmManager;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.os.Build;

import org.json.JSONArray;
import org.json.JSONObject;

/**
 * Reminders that fire while the app is closed.
 *
 * The Rust side hands over the upcoming reminders as JSON ({@code [{at, title, body, tag}]}, {@code at} = epoch milliseconds). They are
 * stored in SharedPreferences and armed as exact, doze-proof {@link AlarmManager} alarms; {@link ReminderReceiver} shows the
 * notification. After a reboot / update / clock or time-zone change {@link BootReceiver} re-arms everything from the stored copy.
 */
final class Reminders {
    static final String CHANNEL = "lumen-reminders-v2";
    static final String PREFS = "lumen_reminders";
    static final String KEY_JSON = "schedule";
    static final String KEY_ARMED = "armed";
    static final String ACTION_FIRE = "com.lumen.app.REMINDER_FIRE";
    private static final int MAX_ALARMS = 60;

    private Reminders() {
    }

    static void store(Context c, String json) {
        SharedPreferences p = c.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
        p.edit().putString(KEY_JSON, json == null ? "[]" : json).apply();
        arm(c);
    }

    static int code(String tag) {
        return (tag == null ? "" : tag).hashCode() & 0x7fffffff;
    }

    private static PendingIntent fire(Context c, String title, String body, String tag, int flags) {
        Intent i = new Intent(c, ReminderReceiver.class).setAction(ACTION_FIRE);
        if (title != null) {
            i.putExtra("title", title).putExtra("body", body).putExtra("tag", tag);
        }
        return PendingIntent.getBroadcast(c, code(tag), i, flags | PendingIntent.FLAG_IMMUTABLE);
    }

    /** Cancels what was armed before and arms the stored schedule (soonest {@value #MAX_ALARMS}). */
    static synchronized void arm(Context c) {
        try {
            AlarmManager am = (AlarmManager) c.getSystemService(Context.ALARM_SERVICE);
            if (am == null) return;
            SharedPreferences p = c.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
            // 1. cancel the previous generation
            JSONArray old = new JSONArray(p.getString(KEY_ARMED, "[]"));
            for (int i = 0; i < old.length(); i++) {
                String tag = old.getString(i);
                PendingIntent pi = fire(c, null, null, tag, PendingIntent.FLAG_NO_CREATE);
                if (pi != null) {
                    am.cancel(pi);
                    pi.cancel();
                }
            }
            // 2. arm the new one
            JSONArray items = new JSONArray(p.getString(KEY_JSON, "[]"));
            JSONArray armed = new JSONArray();
            long now = System.currentTimeMillis();
            boolean exact = Build.VERSION.SDK_INT < 31 || am.canScheduleExactAlarms();
            int n = 0;
            for (int i = 0; i < items.length() && n < MAX_ALARMS; i++) {
                JSONObject it = items.getJSONObject(i);
                long at = (long) it.getDouble("at");
                if (at < now - 90_000L) continue;               // long gone
                String tag = it.optString("tag", "lumen-" + at);
                PendingIntent pi = fire(c, it.optString("title", "Reminder"), it.optString("body", ""), tag, PendingIntent.FLAG_UPDATE_CURRENT);
                long when = Math.max(at, now + 1500L);
                if (exact) {
                    am.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, when, pi);
                } else {
                    am.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, when, pi);
                }
                armed.put(tag);
                n++;
            }
            p.edit().putString(KEY_ARMED, armed.toString()).apply();
        } catch (Throwable ignored) {
        }
    }

    static boolean canPost(Context c) {
        return Build.VERSION.SDK_INT < 33 || c.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED;
    }

    /** Posts a heads-up notification; the same tag replaces the previous one (no duplicates when the app is open too). */
    static void show(Context c, String title, String body, String tag) {
        try {
            if (!canPost(c)) return;
            NotificationManager nm = (NotificationManager) c.getSystemService(Context.NOTIFICATION_SERVICE);
            if (nm == null) return;
            if (Build.VERSION.SDK_INT >= 26) {
                NotificationChannel ch = new NotificationChannel(CHANNEL, "Reminders", NotificationManager.IMPORTANCE_HIGH);
                ch.setDescription("Reminders and automation notices, also while Lumen is closed");
                ch.enableVibration(true);
                nm.createNotificationChannel(ch);
            }
            Intent open = new Intent(c, LumenActivity.class);
            open.addFlags(Intent.FLAG_ACTIVITY_SINGLE_TOP | Intent.FLAG_ACTIVITY_CLEAR_TOP | Intent.FLAG_ACTIVITY_NEW_TASK);
            PendingIntent pi = PendingIntent.getActivity(c, 0, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
            int icon = c.getResources().getIdentifier("ic_stat_lumen", "drawable", c.getPackageName());
            if (icon == 0) icon = c.getApplicationInfo().icon;
            android.app.Notification.Builder b = new android.app.Notification.Builder(c, CHANNEL)
                    .setSmallIcon(icon)
                    .setContentTitle(title)
                    .setContentText(body)
                    .setStyle(new android.app.Notification.BigTextStyle().bigText(body))
                    .setCategory(android.app.Notification.CATEGORY_REMINDER)
                    .setContentIntent(pi)
                    .setAutoCancel(true);
            nm.notify(tag == null || tag.isEmpty() ? "lumen" : tag, 1, b.build());
        } catch (Throwable ignored) {
        }
    }
}
