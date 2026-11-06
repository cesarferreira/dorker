package com.dorker.sample;

import android.app.Activity;
import android.app.Instrumentation;
import android.os.Bundle;
import java.io.File;
import java.io.FileOutputStream;
import java.nio.charset.StandardCharsets;
import java.nio.file.Files;
import java.util.ArrayList;
import java.util.List;

/**
 * A dependency-free instrumentation runner that reports two passing tests and one
 * failing test through the standard instrumentation status protocol. Select tests
 * with `dorker test --class com.dorker.sample.SampleTests#passing` (or `#failing`).
 *
 * Like AndroidJUnitRunner it honours `-e numShards N -e shardIndex I`: test k of the
 * selection runs in shard k % N. Each shard also leaves a marker in the app's data; if it
 * finds one from a different shard, the workers share writable state and it reports a
 * failing `isolatedFromShard<I>` test.
 */
public class SampleRunner extends Instrumentation {
    private String filter = "";
    private int numShards = 1;
    private int shardIndex = 0;
    private String shardKey = "none";

    @Override
    public void onCreate(Bundle args) {
        super.onCreate(args);
        String c = args == null ? null : args.getString("class");
        filter = c == null ? "" : c;
        String n = args == null ? null : args.getString("numShards");
        String i = args == null ? null : args.getString("shardIndex");
        if (n != null && i != null) {
            numShards = Integer.parseInt(n);
            shardIndex = Integer.parseInt(i);
            shardKey = n + "/" + i;
        }
        start();
    }

    @Override
    public void onStart() {
        super.onStart();
        List<String> selected = new ArrayList<>();
        for (String t : new String[] {"passing", "alsoPassing", "failing"}) {
            if (!filter.isEmpty() && !filter.contains(t)) continue;
            // "passing" is a substring of "alsoPassing" only case-insensitively; keep exact:
            if (!filter.isEmpty() && filter.contains("#") && !filter.endsWith("#" + t)) continue;
            selected.add(t);
        }
        String leaked = checkIsolation();
        if (leaked != null) {
            String t = "isolatedFromShard" + leaked.replace('/', '_');
            report(1, t, null);
            report(-2, t, "java.lang.AssertionError: found the marker of shard " + leaked);
        }
        for (int k = 0; k < selected.size(); k++) {
            if (k % numShards != shardIndex) continue;
            String t = selected.get(k);
            report(1, t, null);
            if (t.equals("failing")) {
                report(-2, t, "java.lang.AssertionError: expected 1 but was 2\n\tat com.dorker.sample.SampleTests.failing(SampleTests.java:42)");
            } else {
                report(0, t, null);
            }
        }
        finish(Activity.RESULT_OK, new Bundle());
    }

    /** The other shard whose marker is already here, or null. */
    private String checkIsolation() {
        File marker = new File(getTargetContext().getFilesDir(), "dorker-shard");
        try {
            if (marker.exists()) {
                String other = new String(Files.readAllBytes(marker.toPath()), StandardCharsets.UTF_8);
                if (!other.equals(shardKey)) return other;
            }
            try (FileOutputStream out = new FileOutputStream(marker)) {
                out.write(shardKey.getBytes(StandardCharsets.UTF_8));
            }
        } catch (Exception e) {
            return null;
        }
        return null;
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
