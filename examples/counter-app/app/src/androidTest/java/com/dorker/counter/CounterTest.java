package com.dorker.counter;

import static androidx.test.espresso.Espresso.onView;
import static androidx.test.espresso.action.ViewActions.click;
import static androidx.test.espresso.assertion.ViewAssertions.matches;
import static androidx.test.espresso.matcher.ViewMatchers.isDisplayed;
import static androidx.test.espresso.matcher.ViewMatchers.withId;
import static androidx.test.espresso.matcher.ViewMatchers.withText;
import static org.junit.Assert.assertEquals;

import android.content.Context;

import androidx.test.core.app.ActivityScenario;
import androidx.test.ext.junit.runners.AndroidJUnit4;
import androidx.test.platform.app.InstrumentationRegistry;

import org.junit.Before;
import org.junit.Test;
import org.junit.runner.RunWith;

@RunWith(AndroidJUnit4.class)
public class CounterTest {
    @Before
    public void clearState() {
        prefs().edit().clear().commit();
    }

    private static android.content.SharedPreferences prefs() {
        Context c = InstrumentationRegistry.getInstrumentation().getTargetContext();
        return c.getSharedPreferences(MainActivity.PREFS, Context.MODE_PRIVATE);
    }

    private static void clickIncrement(int times) {
        for (int i = 0; i < times; i++) {
            onView(withId(R.id.increment)).perform(click());
        }
    }

    @Test
    public void startsAtZero() {
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            onView(withId(R.id.count)).check(matches(withText("0")));
        }
    }

    @Test
    public void incrementsOnClick() {
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            clickIncrement(2);
            onView(withId(R.id.count)).check(matches(withText("2")));
        }
    }

    @Test
    public void resetGoesBackToZero() {
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            clickIncrement(1);
            onView(withId(R.id.reset)).perform(click());
            onView(withId(R.id.count)).check(matches(withText("0")));
        }
    }

    @Test
    public void countsPastTen() {
        prefs().edit().putInt(MainActivity.KEY, 9).commit();
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            clickIncrement(2);
            onView(withId(R.id.count)).check(matches(withText("11")));
        }
    }

    @Test
    public void incrementsAgainAfterReset() {
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            clickIncrement(1);
            onView(withId(R.id.reset)).perform(click());
            clickIncrement(1);
            onView(withId(R.id.count)).check(matches(withText("1")));
        }
    }

    @Test
    public void survivesRecreation() {
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            clickIncrement(2);
            s.recreate();
            onView(withId(R.id.count)).check(matches(withText("2")));
        }
    }

    @Test
    public void survivesRelaunch() {
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            clickIncrement(2);
        }
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            onView(withId(R.id.count)).check(matches(withText("2")));
        }
    }

    @Test
    public void persistsToSharedPreferences() {
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            clickIncrement(1);
        }
        assertEquals(1, prefs().getInt(MainActivity.KEY, 0));
    }

    @Test
    public void showsStoredValueOnLaunch() {
        prefs().edit().putInt(MainActivity.KEY, 41).commit();
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            onView(withId(R.id.count)).check(matches(withText("41")));
            clickIncrement(1);
            onView(withId(R.id.count)).check(matches(withText("42")));
        }
    }

    @Test
    public void buttonsAreLabelled() {
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            onView(withText(R.string.increment)).check(matches(isDisplayed()));
            onView(withText(R.string.reset)).check(matches(isDisplayed()));
        }
    }
}
