package com.dorker.counter;

import static org.junit.Assert.assertFalse;

import androidx.test.ext.junit.runners.AndroidJUnit4;

import org.junit.Test;
import org.junit.runner.RunWith;

/**
 * Passes unless the APKs are built with {@code ./gradlew -PdorkerDemoFailure ...}, which
 * makes it fail on purpose so a failing shard, its report and its screenshot can be seen.
 */
@RunWith(AndroidJUnit4.class)
public class DemoFailureTest {
    @Test
    public void deliberateFailureWhenOptedIn() {
        assertFalse(
                "deliberate failure: the APKs were built with -PdorkerDemoFailure",
                BuildConfig.DEMO_FAILURE);
    }
}
