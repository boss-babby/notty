package com.lumen.app;

import android.content.Context;

/**
 * Push for a closed app (site: /api/collab/push). The Firebase SDK is NOT bundled (it needs your google-services.json and a Gradle build);
 * this class looks for it by reflection, so the APK builds and runs either way: without the SDK {@link #available()} is false and the
 * app falls back to the foreground service. With the SDK on the class path, {@link #token} hands the registration token to Rust, which
 * registers it with the site.
 */
public final class FcmKit {
    public interface Callback { void onToken(String token); void onUnavailable(String why); }

    private FcmKit() { }

    public static boolean available() {
        try {
            Class.forName("com.google.firebase.messaging.FirebaseMessaging");
            return true;
        } catch (Throwable t) {
            return false;
        }
    }

    public static void token(Context ctx, final Callback cb) {
        try {
            Class<?> c = Class.forName("com.google.firebase.messaging.FirebaseMessaging");
            Object fm = c.getMethod("getInstance").invoke(null);
            Object task = c.getMethod("getToken").invoke(fm);
            Class<?> t = Class.forName("com.google.android.gms.tasks.Task");
            Class<?> ok = Class.forName("com.google.android.gms.tasks.OnSuccessListener");
            Object listener = java.lang.reflect.Proxy.newProxyInstance(ok.getClassLoader(), new Class<?>[] { ok }, new java.lang.reflect.InvocationHandler() {
                @Override public Object invoke(Object proxy, java.lang.reflect.Method m, Object[] args) {
                    if ("onSuccess".equals(m.getName()) && args != null && args.length == 1 && args[0] != null) cb.onToken(String.valueOf(args[0]));
                    return null;
                }
            });
            t.getMethod("addOnSuccessListener", ok).invoke(task, listener);
        } catch (Throwable e) {
            cb.onUnavailable("Firebase Messaging is not part of this build");
        }
    }
}
