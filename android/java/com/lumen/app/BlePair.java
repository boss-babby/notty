package com.lumen.app;

import android.annotation.SuppressLint;
import android.bluetooth.BluetoothAdapter;
import android.bluetooth.BluetoothDevice;
import android.bluetooth.BluetoothGatt;
import android.bluetooth.BluetoothGattCallback;
import android.bluetooth.BluetoothGattCharacteristic;
import android.bluetooth.BluetoothGattDescriptor;
import android.bluetooth.BluetoothGattServer;
import android.bluetooth.BluetoothGattServerCallback;
import android.bluetooth.BluetoothGattService;
import android.bluetooth.BluetoothManager;
import android.bluetooth.BluetoothProfile;
import android.bluetooth.le.AdvertiseCallback;
import android.bluetooth.le.AdvertiseData;
import android.bluetooth.le.AdvertiseSettings;
import android.bluetooth.le.BluetoothLeAdvertiser;
import android.bluetooth.le.BluetoothLeScanner;
import android.bluetooth.le.ScanCallback;
import android.bluetooth.le.ScanFilter;
import android.bluetooth.le.ScanResult;
import android.bluetooth.le.ScanSettings;
import android.content.Context;
import android.os.ParcelUuid;
import java.util.ArrayList;
import java.util.List;
import java.util.UUID;

/**
 * Bluetooth LE transport for the pairing core in Rust (src/collab/pair.rs): the host advertises the 16-byte session tag and runs a GATT
 * server; a guest scans for the tag and talks to the server. Both directions carry the <b>fragments</b> that pair.rs produces
 * (MTU-sized, any order, CRC) — this class moves bytes and knows nothing about keys, codes or tokens. Written against the six functions
 * of 15.4; it has been compiled into the APK but not run against two radios (see COLLAB.md 16).
 */
@SuppressLint("MissingPermission")
public final class BlePair {
    public interface Listener {
        /** Host: a guest wrote a fragment / Guest: the host notified one. */
        void onFragment(byte[] data);
        /** Guest: a host advertising a session tag was found. */
        void onFound(byte[] tag, String address);
        void onState(String state);
    }

    public static final UUID SERVICE = UUID.fromString("4c756d65-6e00-4000-8000-00805f9b34fb");
    public static final UUID RX = UUID.fromString("4c756d65-6e01-4000-8000-00805f9b34fb"); // guest -> host (write)
    public static final UUID TX = UUID.fromString("4c756d65-6e02-4000-8000-00805f9b34fb"); // host -> guest (notify)
    private static final UUID CCCD = UUID.fromString("00002902-0000-1000-8000-00805f9b34fb");
    private static final int MANUFACTURER = 0x4C55;

    private final Context ctx;
    private final Listener listener;
    private BluetoothGattServer server;
    private BluetoothDevice guest;
    private BluetoothGattCharacteristic tx;
    private BluetoothLeAdvertiser advertiser;
    private BluetoothLeScanner scanner;
    private BluetoothGatt client;
    private BluetoothGattCharacteristic clientRx;
    private final List<byte[]> outgoing = new ArrayList<>();
    private boolean writing;

    public BlePair(Context ctx, Listener l) {
        this.ctx = ctx.getApplicationContext();
        this.listener = l;
    }

    private BluetoothAdapter adapter() {
        BluetoothManager m = (BluetoothManager) ctx.getSystemService(Context.BLUETOOTH_SERVICE);
        BluetoothAdapter a = m == null ? null : m.getAdapter();
        return a != null && a.isEnabled() ? a : null;
    }

    // ───────────── host ─────────────

    public boolean startHost(byte[] tag16) {
        BluetoothAdapter a = adapter();
        if (a == null || tag16 == null || tag16.length != 16) {
            listener.onState("Bluetooth is off or unavailable");
            return false;
        }
        BluetoothManager m = (BluetoothManager) ctx.getSystemService(Context.BLUETOOTH_SERVICE);
        server = m.openGattServer(ctx, new BluetoothGattServerCallback() {
            @Override public void onConnectionStateChange(BluetoothDevice d, int status, int state) {
                if (state == BluetoothProfile.STATE_CONNECTED) { guest = d; listener.onState("A device connected"); }
                else if (d.equals(guest)) { guest = null; listener.onState("The device left"); }
            }
            @Override public void onCharacteristicWriteRequest(BluetoothDevice d, int id, BluetoothGattCharacteristic c, boolean prep, boolean resp, int off, byte[] value) {
                if (RX.equals(c.getUuid()) && value != null) listener.onFragment(value);
                if (resp) server.sendResponse(d, id, 0, 0, null);
            }
            @Override public void onDescriptorWriteRequest(BluetoothDevice d, int id, BluetoothGattDescriptor desc, boolean prep, boolean resp, int off, byte[] value) {
                if (resp) server.sendResponse(d, id, 0, 0, null);
            }
        });
        BluetoothGattService svc = new BluetoothGattService(SERVICE, BluetoothGattService.SERVICE_TYPE_PRIMARY);
        BluetoothGattCharacteristic rx = new BluetoothGattCharacteristic(RX, BluetoothGattCharacteristic.PROPERTY_WRITE, BluetoothGattCharacteristic.PERMISSION_WRITE);
        tx = new BluetoothGattCharacteristic(TX, BluetoothGattCharacteristic.PROPERTY_NOTIFY, BluetoothGattCharacteristic.PERMISSION_READ);
        tx.addDescriptor(new BluetoothGattDescriptor(CCCD, BluetoothGattDescriptor.PERMISSION_READ | BluetoothGattDescriptor.PERMISSION_WRITE));
        svc.addCharacteristic(rx);
        svc.addCharacteristic(tx);
        server.addService(svc);
        advertiser = a.getBluetoothLeAdvertiser();
        if (advertiser == null) { listener.onState("This phone cannot advertise over Bluetooth LE"); return false; }
        AdvertiseSettings s = new AdvertiseSettings.Builder().setAdvertiseMode(AdvertiseSettings.ADVERTISE_MODE_LOW_LATENCY).setConnectable(true).setTimeout(0).build();
        AdvertiseData data = new AdvertiseData.Builder().setIncludeDeviceName(false).addServiceUuid(new ParcelUuid(SERVICE)).build();
        AdvertiseData scan = new AdvertiseData.Builder().setIncludeDeviceName(false).addManufacturerData(MANUFACTURER, tag16).build();
        advertiser.startAdvertising(s, data, scan, new AdvertiseCallback() {
            @Override public void onStartFailure(int code) { listener.onState("Bluetooth advertising failed (" + code + ")"); }
        });
        listener.onState("Waiting for a nearby device…");
        return true;
    }

    /** Host → guest: one fragment as a notification (fragments are ≤ MTU). */
    public boolean notifyGuest(byte[] fragment) {
        if (server == null || guest == null || tx == null) return false;
        tx.setValue(fragment);
        return server.notifyCharacteristicChanged(guest, tx, false);
    }

    // ───────────── guest ─────────────

    public boolean startScan() {
        BluetoothAdapter a = adapter();
        if (a == null) { listener.onState("Bluetooth is off or unavailable"); return false; }
        scanner = a.getBluetoothLeScanner();
        if (scanner == null) return false;
        List<ScanFilter> filters = new ArrayList<>();
        filters.add(new ScanFilter.Builder().setServiceUuid(new ParcelUuid(SERVICE)).build());
        scanner.startScan(filters, new ScanSettings.Builder().setScanMode(ScanSettings.SCAN_MODE_LOW_LATENCY).build(), scanCb);
        listener.onState("Looking for nearby devices…");
        return true;
    }

    private final ScanCallback scanCb = new ScanCallback() {
        @Override public void onScanResult(int type, ScanResult r) {
            byte[] tag = r.getScanRecord() == null ? null : r.getScanRecord().getManufacturerSpecificData(MANUFACTURER);
            if (tag != null && tag.length == 16) listener.onFound(tag, r.getDevice().getAddress());
        }
    };

    public void connect(String address) {
        BluetoothAdapter a = adapter();
        if (a == null) return;
        stopScan();
        client = a.getRemoteDevice(address).connectGatt(ctx, false, new BluetoothGattCallback() {
            @Override public void onConnectionStateChange(BluetoothGatt g, int status, int state) {
                if (state == BluetoothProfile.STATE_CONNECTED) { g.requestMtu(185); }
                else { listener.onState("Disconnected"); }
            }
            @Override public void onMtuChanged(BluetoothGatt g, int mtu, int status) { g.discoverServices(); }
            @Override public void onServicesDiscovered(BluetoothGatt g, int status) {
                BluetoothGattService svc = g.getService(SERVICE);
                if (svc == null) { listener.onState("Not a Lumen device"); return; }
                clientRx = svc.getCharacteristic(RX);
                BluetoothGattCharacteristic t = svc.getCharacteristic(TX);
                g.setCharacteristicNotification(t, true);
                BluetoothGattDescriptor d = t.getDescriptor(CCCD);
                d.setValue(BluetoothGattDescriptor.ENABLE_NOTIFICATION_VALUE);
                g.writeDescriptor(d);
                listener.onState("Connected — enter the code on both devices");
            }
            @Override public void onCharacteristicChanged(BluetoothGatt g, BluetoothGattCharacteristic c) {
                byte[] v = c.getValue();
                if (v != null) listener.onFragment(v);
            }
            @Override public void onCharacteristicWrite(BluetoothGatt g, BluetoothGattCharacteristic c, int status) {
                synchronized (outgoing) { writing = false; }
                pump();
            }
        });
    }

    /** Guest → host: queued, written one at a time (GATT allows a single outstanding write). */
    public void writeToHost(byte[] fragment) {
        synchronized (outgoing) { outgoing.add(fragment); }
        pump();
    }

    private void pump() {
        byte[] next;
        synchronized (outgoing) {
            if (writing || outgoing.isEmpty() || client == null || clientRx == null) return;
            next = outgoing.remove(0);
            writing = true;
        }
        clientRx.setValue(next);
        clientRx.setWriteType(BluetoothGattCharacteristic.WRITE_TYPE_DEFAULT);
        client.writeCharacteristic(clientRx);
    }

    public void stopScan() {
        if (scanner != null) { try { scanner.stopScan(scanCb); } catch (Exception ignored) { } scanner = null; }
    }

    public void stop() {
        stopScan();
        if (advertiser != null) { try { advertiser.stopAdvertising(new AdvertiseCallback() { }); } catch (Exception ignored) { } advertiser = null; }
        if (server != null) { server.close(); server = null; }
        if (client != null) { client.close(); client = null; }
        guest = null;
    }
}
