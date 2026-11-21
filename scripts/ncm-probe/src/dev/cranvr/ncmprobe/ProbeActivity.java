package dev.cranvr.ncmprobe;

import android.app.Activity;
import android.content.Context;
import android.net.ConnectivityManager;
import android.net.LinkAddress;
import android.net.LinkProperties;
import android.net.Network;
import android.net.NetworkCapabilities;
import android.net.NetworkRequest;
import android.net.RouteInfo;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.util.Log;
import android.widget.TextView;

import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.net.DatagramPacket;
import java.net.DatagramSocket;
import java.net.Inet6Address;
import java.net.InetAddress;
import java.net.InetSocketAddress;
import java.net.NetworkInterface;
import java.net.Socket;
import java.nio.charset.StandardCharsets;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

/**
 * A test app for the headset's USB network (scripts/ncm-probe/run.sh). It asks for the network,
 * logs what the system hands out, tries the link to the Mac and gives the network back after the
 * hold time. Each log line also goes to the Mac over Wi-Fi, because USB adb drops while the
 * headset changes its USB functions.
 */
public class ProbeActivity extends Activity {
    static final String TAG = "NcmProbe";
    static final int PORT = 9963;
    static final int LOG_PORT = 9964;

    private final ExecutorService logIo = Executors.newSingleThreadExecutor();
    private final ExecutorService testIo = Executors.newSingleThreadExecutor();
    private final Handler handler = new Handler(Looper.getMainLooper());
    private DatagramSocket logSocket;
    private InetAddress logTo;
    private long t0;
    private TextView text;
    private final StringBuilder shown = new StringBuilder();
    private ConnectivityManager cm;
    private ConnectivityManager.NetworkCallback callback;
    private boolean released;

    void log(String message) {
        String line = String.format("%.3f %s", (SystemClock.elapsedRealtime() - t0) / 1000.0, message);
        Log.i(TAG, line);
        try (FileOutputStream out = openFileOutput("probe.log", Context.MODE_APPEND)) {
            out.write((line + "\n").getBytes(StandardCharsets.UTF_8));
        } catch (Exception ignored) {
        }
        handler.post(() -> {
            shown.append(line).append('\n');
            if (text != null) text.setText(shown);
        });
        if (logTo != null) {
            logIo.execute(() -> {
                try {
                    if (logSocket == null) logSocket = new DatagramSocket();
                    byte[] bytes = line.getBytes(StandardCharsets.UTF_8);
                    logSocket.send(new DatagramPacket(bytes, bytes.length, logTo, LOG_PORT));
                } catch (Exception ignored) {
                }
            });
        }
    }

    @Override
    protected void onCreate(Bundle state) {
        super.onCreate(state);
        t0 = SystemClock.elapsedRealtime();
        text = new TextView(this);
        text.setTextSize(14);
        setContentView(text);
        String mac = getIntent().getStringExtra("mac");
        int hold = getIntent().getIntExtra("hold", 60);
        try {
            if (mac != null) logTo = InetAddress.getByName(mac);
        } catch (Exception e) {
            Log.w(TAG, "bad mac address " + mac);
        }
        log("start sdk=" + Build.VERSION.SDK_INT + " hold=" + hold + "s");
        cm = (ConnectivityManager) getSystemService(Context.CONNECTIVITY_SERVICE);
        for (Network n : cm.getAllNetworks()) {
            log("existing " + n + " caps=" + cm.getNetworkCapabilities(n) + " iface=" + iface(cm.getLinkProperties(n)));
        }
        NetworkRequest request = new NetworkRequest.Builder()
                .addTransportType(NetworkCapabilities.TRANSPORT_USB)
                .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .removeCapability(NetworkCapabilities.NET_CAPABILITY_TRUSTED)
                .build();
        log("request " + request);
        callback = new ConnectivityManager.NetworkCallback() {
            @Override
            public void onAvailable(Network network) {
                log("onAvailable " + network + " handle=" + network.getNetworkHandle());
                testIo.execute(() -> test(network));
            }

            @Override
            public void onUnavailable() {
                log("onUnavailable");
            }

            @Override
            public void onLost(Network network) {
                log("onLost " + network);
            }

            @Override
            public void onLosing(Network network, int maxMsToLive) {
                log("onLosing " + network + " " + maxMsToLive);
            }

            @Override
            public void onCapabilitiesChanged(Network network, NetworkCapabilities caps) {
                log("caps " + network + " down=" + caps.getLinkDownstreamBandwidthKbps() + "kbps up="
                        + caps.getLinkUpstreamBandwidthKbps() + "kbps " + caps);
            }

            @Override
            public void onLinkPropertiesChanged(Network network, LinkProperties lp) {
                log("link " + network + " " + iface(lp));
            }

            @Override
            public void onBlockedStatusChanged(Network network, boolean blocked) {
                log("blocked " + network + " " + blocked);
            }
        };
        try {
            cm.requestNetwork(request, callback);
            log("requested");
        } catch (Exception e) {
            log("request failed " + e);
        }
        handler.postDelayed(this::release, hold * 1000L);
    }

    static String iface(LinkProperties lp) {
        if (lp == null) return "none";
        StringBuilder out = new StringBuilder(String.valueOf(lp.getInterfaceName()));
        for (LinkAddress a : lp.getLinkAddresses()) out.append(" addr=").append(a);
        for (RouteInfo r : lp.getRoutes()) out.append(" route=").append(r);
        out.append(" mtu=").append(lp.getMtu());
        return out.toString();
    }

    void test(Network network) {
        try {
            SystemClock.sleep(1500);
            LinkProperties lp = cm.getLinkProperties(network);
            String name = lp == null ? null : lp.getInterfaceName();
            log("test on " + name);
            NetworkInterface ni = name == null ? null : NetworkInterface.getByName(name);
            log("interface index=" + (ni == null ? -1 : ni.getIndex()) + " up=" + (ni != null && ni.isUp()));
            Inet6Address group = Inet6Address.getByAddress(null, InetAddress.getByName("ff02::1").getAddress(), ni);
            InetAddress peer = null;
            for (int round = 0; round < 2 && peer == null; round++) {
                boolean bound = round == 0;
                try (DatagramSocket ds = new DatagramSocket(new InetSocketAddress("::", 0))) {
                    if (bound) network.bindSocket(ds);
                    ds.setSoTimeout(500);
                    byte[] ask = ("NCMPROBE? bound=" + bound).getBytes(StandardCharsets.UTF_8);
                    long end = SystemClock.elapsedRealtime() + 3000;
                    while (SystemClock.elapsedRealtime() < end) {
                        try {
                            ds.send(new DatagramPacket(ask, ask.length, group, PORT));
                        } catch (Exception e) {
                            log("multicast send bound=" + bound + " failed " + e);
                            SystemClock.sleep(500);
                            continue;
                        }
                        try {
                            byte[] buf = new byte[512];
                            DatagramPacket p = new DatagramPacket(buf, buf.length);
                            ds.receive(p);
                            log("reply bound=" + bound + " from " + p.getAddress() + " port " + p.getPort() + ": "
                                    + new String(buf, 0, p.getLength(), StandardCharsets.UTF_8));
                            if (peer == null) peer = p.getAddress();
                        } catch (java.net.SocketTimeoutException ignored) {
                        }
                    }
                } catch (Exception e) {
                    log("udp bound=" + bound + " failed " + e);
                }
            }
            if (peer == null) {
                log("no peer answered the multicast");
                return;
            }
            log("peer " + peer);
            // TCP through the network's factory, then an unbound socket with the scope id
            for (int round = 0; round < 2; round++) {
                boolean bound = round == 0;
                try (Socket s = bound ? network.getSocketFactory().createSocket() : new Socket()) {
                    long start = SystemClock.elapsedRealtime();
                    s.connect(new InetSocketAddress(peer, PORT), 3000);
                    log("tcp bound=" + bound + " connected in " + (SystemClock.elapsedRealtime() - start) + " ms local=" + s.getLocalSocketAddress());
                    s.setSoTimeout(5000);
                    OutputStream out = s.getOutputStream();
                    InputStream in = s.getInputStream();
                    out.write((bound ? "blast\n" : "hello\n").getBytes(StandardCharsets.UTF_8));
                    out.flush();
                    byte[] buf = new byte[1 << 16];
                    long total = 0;
                    long begin = SystemClock.elapsedRealtime();
                    int n;
                    while ((n = in.read(buf)) > 0) total += n;
                    long ms = SystemClock.elapsedRealtime() - begin;
                    log("tcp bound=" + bound + " read " + total + " bytes in " + ms + " ms = "
                            + String.format("%.1f", total * 8.0 / 1000.0 / Math.max(ms, 1)) + " Mbps");
                } catch (Exception e) {
                    log("tcp bound=" + bound + " failed " + e);
                }
            }
            // A ping-like round trip over UDP to the peer, bound
            try (DatagramSocket ds = new DatagramSocket(new InetSocketAddress("::", 0))) {
                network.bindSocket(ds);
                ds.setSoTimeout(500);
                long[] rtts = new long[50];
                int got = 0;
                for (int i = 0; i < rtts.length; i++) {
                    byte[] ask = ("NCMPING " + i).getBytes(StandardCharsets.UTF_8);
                    long start = System.nanoTime();
                    ds.send(new DatagramPacket(ask, ask.length, peer, PORT));
                    try {
                        byte[] buf = new byte[512];
                        DatagramPacket p = new DatagramPacket(buf, buf.length);
                        ds.receive(p);
                        rtts[got++] = (System.nanoTime() - start) / 1000;
                    } catch (java.net.SocketTimeoutException ignored) {
                    }
                    SystemClock.sleep(20);
                }
                java.util.Arrays.sort(rtts, 0, got);
                if (got > 0) log("udp rtt us: n=" + got + " p50=" + rtts[got / 2] + " p90=" + rtts[got * 9 / 10] + " max=" + rtts[got - 1]);
            } catch (Exception e) {
                log("udp rtt failed " + e);
            }
        } catch (Exception e) {
            log("test failed " + e);
        }
    }

    void release() {
        if (released) return;
        released = true;
        try {
            cm.unregisterNetworkCallback(callback);
            log("released");
        } catch (Exception e) {
            log("release failed " + e);
        }
        handler.postDelayed(this::finish, 3000);
    }

    // A system dialog over the app takes the window's focus. While USB adb is down, these lines
    // reach the Mac over Wi-Fi and show whether the headset asked its user.
    @Override
    protected void onResume() {
        super.onResume();
        log("resumed");
    }

    @Override
    protected void onPause() {
        log("paused");
        super.onPause();
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        log("window focus " + hasFocus);
    }

    @Override
    protected void onDestroy() {
        release();
        super.onDestroy();
    }
}
