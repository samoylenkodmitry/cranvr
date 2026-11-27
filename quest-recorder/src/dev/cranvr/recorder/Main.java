package dev.cranvr.recorder;

import android.content.ComponentName;
import android.content.Intent;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.Message;
import android.os.Messenger;
import android.os.SystemClock;
import java.lang.reflect.Method;

/**
 * Starts or stops the headset's own recorder (Meta's MetaCam) through its app actions. The cranvr app
 * on the Mac runs it over adb as the shell user, which holds the recorder's permission:
 * {@code CLASSPATH=cranvr-quest-recorder.jar app_process / dev.cranvr.recorder.Main start|stop}.
 *
 * <p>The recorder answers through the messenger of the request. The program prints {@code ok} and the
 * time of the answer on {@code SystemClock.elapsedRealtimeNanos}, or {@code error} and the reason, and
 * ends with code 0 or 1. The recorder keeps its own settings: the program changes none.
 */
public final class Main {
    private static final String ACTION_PREFIX = "horizonos.appactions.";
    private static final ComponentName RECORDER = new ComponentName("com.oculus.metacam", "com.oculus.metacam.capture.CaptureService");
    /** The longest wait for the recorder's answer, in ms */
    private static final long ANSWER_WAIT = 5000;
    /** The answer of a request that the recorder took */
    private static final int RESULT_OK = -1;

    private Main() {}

    public static void main(String[] args) throws Exception {
        String action;
        if (args.length == 1 && args[0].equals("start")) {
            action = ACTION_PREFIX + "START_RECORDING";
        } else if (args.length == 1 && args[0].equals("stop")) {
            action = ACTION_PREFIX + "STOP_RECORDING";
        } else {
            System.out.println("error usage: start or stop");
            System.exit(2);
            return;
        }
        Looper.prepare();
        int[] code = {1};
        Handler handler = new Handler(Looper.myLooper()) {
            @Override
            public void handleMessage(Message message) {
                Bundle data = message.peekData();
                String error = data == null ? null : data.getString("__error_message");
                if (message.arg1 == RESULT_OK && error == null) {
                    System.out.println("ok " + SystemClock.elapsedRealtimeNanos());
                    code[0] = 0;
                } else {
                    System.out.println("error " + (error != null ? error : "the recorder answered " + message.arg1));
                }
                Looper.myLooper().quit();
            }
        };
        Intent intent = new Intent(action);
        intent.setComponent(RECORDER);
        intent.putExtra("__result_messenger", new Messenger(handler));
        intent.putExtra("__request_id", "cranvr-" + SystemClock.elapsedRealtimeNanos());
        if (!startService(intent)) {
            System.out.println("error the headset has no recorder that takes the request");
            System.exit(1);
            return;
        }
        handler.postDelayed(() -> {
            System.out.println("error the recorder did not answer within " + ANSWER_WAIT + " ms");
            Looper.myLooper().quit();
        }, ANSWER_WAIT);
        Looper.loop();
        System.exit(code[0]);
    }

    /** Starts the recorder's service as the shell user. Returns whether a service took the intent. */
    private static boolean startService(Intent intent) throws Exception {
        Object manager = Class.forName("android.app.ActivityManager").getMethod("getService").invoke(null);
        Method start = null;
        for (Method method : manager.getClass().getMethods()) {
            if (method.getName().equals("startService")) {
                start = method;
            }
        }
        if (start == null) {
            return false;
        }
        // startService(caller, intent, resolvedType, requireForeground, callingPackage, callingFeatureId, userId)
        Class<?>[] types = start.getParameterTypes();
        Object[] values = new Object[types.length];
        values[1] = intent;
        values[3] = false;
        values[4] = "com.android.shell";
        values[types.length - 1] = 0;
        return start.invoke(manager, values) != null;
    }
}
