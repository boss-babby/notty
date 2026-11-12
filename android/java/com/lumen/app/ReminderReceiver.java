package com.lumen.app;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

/** Fired by AlarmManager at the reminder time — works while the app is closed or the phone is dozing. */
public class ReminderReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context c, Intent i) {
        String title = i.getStringExtra("title");
        String body = i.getStringExtra("body");
        String tag = i.getStringExtra("tag");
        if (title == null) return;
        Reminders.show(c, title, body == null ? "" : body, tag);
    }
}
