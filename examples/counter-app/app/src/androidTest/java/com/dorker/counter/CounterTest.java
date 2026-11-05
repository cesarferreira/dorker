package com.dorker.counter;

import static androidx.test.espresso.Espresso.onView;
import static androidx.test.espresso.action.ViewActions.click;
import static androidx.test.espresso.assertion.ViewAssertions.matches;
import static androidx.test.espresso.matcher.ViewMatchers.withId;
import static androidx.test.espresso.matcher.ViewMatchers.withText;

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
        Context c = InstrumentationRegistry.getInstrumentation().getTargetContext();
        c.getSharedPreferences(MainActivity.PREFS, Context.MODE_PRIVATE).edit().clear().commit();
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
            onView(withId(R.id.increment)).perform(click());
            onView(withId(R.id.increment)).perform(click());
            onView(withId(R.id.count)).check(matches(withText("2")));
        }
    }

    @Test
    public void resetGoesBackToZero() {
        try (ActivityScenario<MainActivity> s = ActivityScenario.launch(MainActivity.class)) {
            onView(withId(R.id.increment)).perform(click());
            onView(withId(R.id.reset)).perform(click());
            onView(withId(R.id.count)).check(matches(withText("0")));
        }
    }
}
