package com.lumen.app;

import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;

import java.security.KeyStore;

import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

/** AES-256-GCM wrapping with a non-exportable key in the Android Keystore (hardware-backed where the device has a TEE / StrongBox). */
final class KeyKit {
    private static final String ALIAS = "lumen_device_key";
    private static final int IV_LEN = 12;

    private KeyKit() {}

    private static SecretKey key() throws Exception {
        KeyStore ks = KeyStore.getInstance("AndroidKeyStore");
        ks.load(null);
        if (!ks.containsAlias(ALIAS)) {
            KeyGenerator g = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
            g.init(new KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .setKeySize(256)
                    .build());
            g.generateKey();
        }
        return (SecretKey) ks.getKey(ALIAS, null);
    }

    /** iv(12) + ciphertext, or null when the keystore is unavailable. */
    static byte[] wrap(byte[] plain) {
        try {
            Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
            c.init(Cipher.ENCRYPT_MODE, key());
            byte[] iv = c.getIV();
            byte[] ct = c.doFinal(plain);
            byte[] out = new byte[iv.length + ct.length];
            System.arraycopy(iv, 0, out, 0, iv.length);
            System.arraycopy(ct, 0, out, iv.length, ct.length);
            return out;
        } catch (Exception e) {
            return null;
        }
    }

    static byte[] unwrap(byte[] blob) {
        try {
            if (blob == null || blob.length <= IV_LEN) return null;
            Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
            c.init(Cipher.DECRYPT_MODE, key(), new GCMParameterSpec(128, blob, 0, IV_LEN));
            return c.doFinal(blob, IV_LEN, blob.length - IV_LEN);
        } catch (Exception e) {
            return null;
        }
    }
}
