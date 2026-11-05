package com.dorker.sample;

import android.app.Activity;
import android.app.Instrumentation;
import android.os.Bundle;

/**
 * A dependency-free instrumentation runner that reports two passing tests and one
 * failing test through the standard instrumentation status protocol. Select tests
 * with `dorker test --class com.dorker.sample.SampleTests#passing` (or `#failing`).
 */
public class SampleRunner extends Instrumentation {
    private String filter = "";

    @Override
    public void onCreate(Bundle args) {
        super.onCreate(args);
        String c = args == null ? null : args.getString("class");
        filter = c == null ? "" : c;
        start();
    }

    @Override
    public void onStart() {
        super.onStart();
        String[] tests = {"passing", "alsoPassing", "failing"};
        int run = 0;
        for (String t : tests) {
            if (!filter.isEmpty() && !filter.contains(t)) continue;
            // "passing" is a substring of "alsoPassing" only case-insensitively; keep exact:
            if (!filter.isEmpty() && filter.contains("#") && !filter.endsWith("#" + t)) continue;
            run++;
            report(1, t, null);
            if (t.equals("failing")) {
                report(-2, t, "java.lang.AssertionError: expected 1 but was 2\n\tat com.dorker.sample.SampleTests.failing(SampleTests.java:42)");
            } else {
                report(0, t, null);
            }
        }
        finish(Activity.RESULT_OK, new Bundle());
    }

    private void report(int code, String test, String stack) {
        Bundle b = new Bundle();
        b.putString("class", "com.dorker.sample.SampleTests");
        b.putString("test", test);
        b.putInt("numtests", 3);
        if (stack != null) b.putString("stack", stack);
        sendStatus(code, b);
    }
}
